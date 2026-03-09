//! Git worktree management for coworker isolation.
//!
//! All worktrees use the task-based layout:
//! `~/.midtown/projects/<repo>/worktrees/<branch-slug>/`
//!
//! Worktrees are named by branch slug (decoupled from coworker identity),
//! enabling build cache reuse across task reassignment.

use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::warn;

use crate::Error;

/// Result of worktree operations
pub type WorktreeResult<T> = std::result::Result<T, WorktreeError>;

/// Errors specific to worktree operations
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("Failed to detect repository: {0}")]
    RepoDetection(String),

    #[error("Worktree does not exist: {0}")]
    NotFound(PathBuf),

    #[error("Git command failed: {0}")]
    GitError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<WorktreeError> for Error {
    fn from(e: WorktreeError) -> Self {
        Error::Io(std::io::Error::other(e.to_string()))
    }
}

/// Manages git worktrees for coworker isolation.
///
/// All worktrees are task-based, stored at `~/.midtown/worktrees/<repo>/<worktree-id>/`.
#[derive(Debug, Clone)]
pub struct WorktreeManager {
    /// Root repository path (the main checkout)
    repo_root: PathBuf,
    /// Repository name
    repo_name: String,
    /// Base path for task-based worktrees (~/.midtown/projects/<repo>/worktrees/)
    task_worktrees_base: PathBuf,
}

impl WorktreeManager {
    /// Create a new worktree manager by detecting the repository from the current directory.
    pub fn from_current_dir() -> WorktreeResult<Self> {
        let repo_root = detect_repo_root()?;
        let repo_name = repo_name_from_path(&repo_root)?;
        let task_worktrees_base = crate::paths::worktrees_dir_for_repo(&repo_name);

        Ok(Self {
            repo_root,
            repo_name,
            task_worktrees_base,
        })
    }

    /// Create a new worktree manager for a specific repository.
    pub fn new(repo_root: PathBuf) -> WorktreeResult<Self> {
        let repo_name = repo_name_from_path(&repo_root)?;
        let task_worktrees_base = crate::paths::worktrees_dir_for_repo(&repo_name);

        Ok(Self {
            repo_root,
            repo_name,
            task_worktrees_base,
        })
    }

    /// Create or update the lead's persistent worktree.
    ///
    /// The lead worktree lives at `~/.midtown/projects/<repo>/worktrees/lead/` and uses
    /// detached HEAD (same as coworkers). Unlike task worktrees, this does NOT
    /// create a branch — the lead creates branches as needed for work.
    ///
    /// If the worktree already exists, it is updated to the current HEAD of the
    /// main repository so that the lead always works against up-to-date code.
    pub fn create_lead_worktree(&self) -> WorktreeResult<PathBuf> {
        let worktree_path = self.task_worktrees_base.join("lead");

        // If the worktree exists and is registered, update it to current HEAD
        if worktree_path.exists() && self.is_worktree_registered(&worktree_path) {
            self.update_lead_worktree(&worktree_path)?;
            crate::settings::ensure_auto_compact_settings(&worktree_path);
            return Ok(worktree_path);
        }

        // Path exists but not registered with git — remove it first
        if worktree_path.exists() {
            let _ = std::fs::remove_dir_all(&worktree_path);
        }

        // Ensure parent directory exists
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Create the worktree detached at HEAD
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args([
                "worktree",
                "add",
                "--detach",
                worktree_path.to_str().unwrap(),
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            // Stale reference — prune and retry once
            if !worktree_path.exists() {
                tracing::warn!(
                    "git worktree add for lead failed ({}), pruning and retrying",
                    stderr.trim()
                );
                let _ = self.prune();

                let retry = Command::new("git")
                    .current_dir(&self.repo_root)
                    .args([
                        "worktree",
                        "add",
                        "--detach",
                        worktree_path.to_str().unwrap(),
                    ])
                    .output()?;

                if !retry.status.success() {
                    return Err(WorktreeError::GitError(
                        String::from_utf8_lossy(&retry.stderr).to_string(),
                    ));
                }

                crate::settings::ensure_auto_compact_settings(&worktree_path);
                return Ok(worktree_path);
            }

            return Err(WorktreeError::GitError(stderr));
        }

        crate::settings::ensure_auto_compact_settings(&worktree_path);
        Ok(worktree_path)
    }

    /// Update the lead worktree to match the main repo's current HEAD.
    ///
    /// Runs `git checkout --detach HEAD` in the worktree, pointing it at the
    /// same commit as the main repository's HEAD. This ensures the lead always
    /// sees the latest code (e.g., after `git pull` in the main repo).
    fn update_lead_worktree(&self, worktree_path: &Path) -> WorktreeResult<()> {
        // Resolve the main repo's HEAD commit
        let head_output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["rev-parse", "HEAD"])
            .output()?;

        if !head_output.status.success() {
            return Err(WorktreeError::GitError(
                String::from_utf8_lossy(&head_output.stderr).to_string(),
            ));
        }

        let head_commit = String::from_utf8_lossy(&head_output.stdout)
            .trim()
            .to_string();

        // Check if worktree is already at this commit
        let wt_head = Command::new("git")
            .current_dir(worktree_path)
            .args(["rev-parse", "HEAD"])
            .output()?;

        if wt_head.status.success() {
            let wt_commit = String::from_utf8_lossy(&wt_head.stdout).trim().to_string();
            if wt_commit == head_commit {
                return Ok(());
            }
        }

        // Update to the main repo's HEAD
        let output = Command::new("git")
            .current_dir(worktree_path)
            .args(["checkout", "--detach", &head_commit])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            warn!(
                "Failed to update lead worktree to HEAD {}: {}",
                &head_commit[..8.min(head_commit.len())],
                stderr.trim()
            );
            return Err(WorktreeError::GitError(stderr));
        }

        Ok(())
    }

    /// Prune stale worktree references.
    ///
    /// Runs `git worktree prune` to clean up worktree administrative files
    /// for worktrees that no longer exist on disk.
    pub fn prune(&self) -> WorktreeResult<()> {
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["worktree", "prune"])
            .output()?;

        if !output.status.success() {
            return Err(WorktreeError::GitError(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(())
    }

    /// Check if a worktree path is registered with git and valid.
    ///
    /// Returns true if the path exists in `git worktree list`, false otherwise.
    fn is_worktree_registered(&self, path: &Path) -> bool {
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["worktree", "list", "--porcelain"])
            .output();

        if let Ok(output) = output
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if line.starts_with("worktree ")
                    && let Some(worktree_path) = line.strip_prefix("worktree ")
                    && paths_match(Path::new(worktree_path), path)
                {
                    return true;
                }
            }
        }
        false
    }

    /// Get the current branch name for a worktree.
    ///
    /// Returns None if the worktree is in detached HEAD state or on an error.
    fn get_worktree_branch(&self, worktree_path: &Path) -> Option<String> {
        let output = Command::new("git")
            .current_dir(worktree_path)
            .args(["branch", "--show-current"])
            .output()
            .ok()?;

        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !branch.is_empty() {
                return Some(branch);
            }
        }
        None
    }

    /// Get the repository root path.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Get the repository name.
    pub fn repo_name(&self) -> &str {
        &self.repo_name
    }

    /// Resolve the start-point for new worktree branches.
    ///
    /// Prefers `origin/<default>` (tracks remote), falls back to local
    /// `<default>`, ultimate fallback `"main"`.
    fn resolve_default_start_point(&self) -> String {
        let default_branch =
            detect_default_branch(&self.repo_root).unwrap_or_else(|| "main".to_string());

        // Prefer origin/<default> so branches track the remote tip
        let remote_ref = format!("origin/{}", default_branch);
        let has_remote = Command::new("git")
            .current_dir(&self.repo_root)
            .args([
                "rev-parse",
                "--verify",
                &format!("refs/remotes/{}", remote_ref),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if has_remote {
            remote_ref
        } else {
            default_branch
        }
    }

    // ========================================================================
    // Task-based worktree operations
    // ========================================================================

    /// Get the path for a task-based worktree.
    ///
    /// Returns `~/.midtown/projects/<repo>/worktrees/<worktree_id>/`.
    pub fn task_worktree_path(&self, worktree_id: &str) -> PathBuf {
        self.task_worktrees_base.join(worktree_id)
    }

    /// Create a detached-HEAD worktree at `~/.midtown/projects/<repo>/worktrees/<worktree_id>/`.
    ///
    /// Unlike `create_task_worktree`, this does NOT create or delete any branches.
    /// Use this for additional repos in multi-repo projects where the worktree_id
    /// (typically a coworker name) could collide with real branch names.
    pub fn create_detached_worktree(&self, worktree_id: &str) -> WorktreeResult<PathBuf> {
        let worktree_path = self.task_worktree_path(worktree_id);

        // Check if worktree already exists and is valid (idempotent)
        if worktree_path.exists() && self.is_worktree_registered(&worktree_path) {
            crate::settings::ensure_auto_compact_settings(&worktree_path);
            return Ok(worktree_path);
        }

        // Path exists but not registered with git - remove it first
        if worktree_path.exists() {
            let _ = std::fs::remove_dir_all(&worktree_path);
        }

        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let start_point = self.resolve_default_start_point();
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args([
                "worktree",
                "add",
                "--detach",
                worktree_path.to_str().unwrap(),
                &start_point,
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            // Stale reference — prune and retry once
            if !worktree_path.exists() {
                tracing::warn!(
                    "git worktree add --detach for {} failed ({}), pruning and retrying",
                    worktree_id,
                    stderr.trim()
                );
                let _ = self.prune();

                let retry = Command::new("git")
                    .current_dir(&self.repo_root)
                    .args([
                        "worktree",
                        "add",
                        "--detach",
                        worktree_path.to_str().unwrap(),
                        &start_point,
                    ])
                    .output()?;

                if !retry.status.success() {
                    return Err(WorktreeError::GitError(
                        String::from_utf8_lossy(&retry.stderr).to_string(),
                    ));
                }

                crate::settings::ensure_auto_compact_settings(&worktree_path);
                return Ok(worktree_path);
            }

            return Err(WorktreeError::GitError(stderr));
        }

        crate::settings::ensure_auto_compact_settings(&worktree_path);
        Ok(worktree_path)
    }

    /// Create a task-based worktree at `~/.midtown/projects/<repo>/worktrees/<worktree_id>/`.
    ///
    /// The worktree is created on a branch matching the worktree_id, starting
    /// from the default branch (not HEAD). This prevents cross-PR contamination
    /// when the lead's HEAD is on an unrelated feature branch.
    pub fn create_task_worktree(&self, worktree_id: &str) -> WorktreeResult<PathBuf> {
        let worktree_path = self.task_worktree_path(worktree_id);
        let start_point = self.resolve_default_start_point();

        // Check if worktree already exists and is valid (idempotent)
        if worktree_path.exists() && self.is_worktree_registered(&worktree_path) {
            // Validate that the existing worktree is on the expected branch
            let actual_branch = self.get_worktree_branch(&worktree_path);
            if actual_branch.as_deref() != Some(worktree_id) {
                return Err(WorktreeError::GitError(format!(
                    "Worktree exists but branch mismatch: expected '{}', got {:?}",
                    worktree_id, actual_branch
                )));
            }
            crate::settings::ensure_auto_compact_settings(&worktree_path);
            return Ok(worktree_path);
        }

        // Path exists but not registered with git - remove it first
        if worktree_path.exists() {
            let _ = std::fs::remove_dir_all(&worktree_path);
        }

        // Check if the branch already exists (from a stale worktree).
        // If so, delete it before attempting to create the worktree.
        let branch_exists = Command::new("git")
            .current_dir(&self.repo_root)
            .args([
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", worktree_id),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if branch_exists {
            tracing::warn!(
                "Branch {} exists but worktree doesn't - likely stale, cleaning up",
                worktree_id
            );
            // Prune stale worktree references first (in case the branch is linked to a deleted worktree)
            let _ = self.prune();

            // Delete the stale branch
            let delete_output = Command::new("git")
                .current_dir(&self.repo_root)
                .args(["branch", "-D", worktree_id])
                .output();

            match delete_output {
                Ok(output) if !output.status.success() => {
                    tracing::warn!(
                        "Failed to delete stale branch {}: {}",
                        worktree_id,
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to run git branch -D {}: {}", worktree_id, e);
                }
                _ => {}
            }
        }

        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Create worktree at the branch (creating branch if needed)
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args([
                "worktree",
                "add",
                "-b",
                worktree_id,
                worktree_path.to_str().unwrap(),
                &start_point,
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            // If the branch already exists, delete it and retry
            // This handles the case where a stale branch is left from a previous
            // worktree that was deleted or moved
            if stderr.contains("already exists") {
                tracing::warn!(
                    "Branch {} already exists, deleting stale branch and retrying",
                    worktree_id
                );

                // Delete the stale branch (force delete since it may not be merged)
                let delete_output = Command::new("git")
                    .current_dir(&self.repo_root)
                    .args(["branch", "-D", worktree_id])
                    .output()?;

                if !delete_output.status.success() {
                    tracing::warn!(
                        "Failed to delete stale branch {}: {}",
                        worktree_id,
                        String::from_utf8_lossy(&delete_output.stderr)
                    );
                }

                // Prune stale worktree references in case the branch was linked
                // to a deleted worktree
                let _ = self.prune();

                // Retry creating the worktree with the branch
                let retry = Command::new("git")
                    .current_dir(&self.repo_root)
                    .args([
                        "worktree",
                        "add",
                        "-b",
                        worktree_id,
                        worktree_path.to_str().unwrap(),
                        &start_point,
                    ])
                    .output()?;

                if !retry.status.success() {
                    return Err(WorktreeError::GitError(
                        String::from_utf8_lossy(&retry.stderr).to_string(),
                    ));
                }
            } else if !worktree_path.exists() {
                // Prune stale references and retry
                tracing::warn!(
                    "git worktree add failed ({}), pruning and retrying",
                    stderr.trim()
                );
                let _ = self.prune();
                let retry = Command::new("git")
                    .current_dir(&self.repo_root)
                    .args([
                        "worktree",
                        "add",
                        "-b",
                        worktree_id,
                        worktree_path.to_str().unwrap(),
                        &start_point,
                    ])
                    .output()?;
                if !retry.status.success() {
                    return Err(WorktreeError::GitError(
                        String::from_utf8_lossy(&retry.stderr).to_string(),
                    ));
                }
            } else {
                return Err(WorktreeError::GitError(stderr));
            }
        }

        // Validate that the worktree is on the expected branch
        // This ensures registry data matches actual git state even if error recovery
        // paths checked out a different branch
        let actual_branch = self.get_worktree_branch(&worktree_path);
        if actual_branch.as_deref() != Some(worktree_id) {
            return Err(WorktreeError::GitError(format!(
                "Worktree created but branch mismatch: expected '{}', got {:?}",
                worktree_id, actual_branch
            )));
        }

        crate::settings::ensure_auto_compact_settings(&worktree_path);
        Ok(worktree_path)
    }

    /// Remove a task-based worktree and its branch.
    pub fn remove_task_worktree(&self, worktree_id: &str, force: bool) -> WorktreeResult<()> {
        let worktree_path = self.task_worktree_path(worktree_id);

        if !worktree_path.exists() {
            return Err(WorktreeError::NotFound(worktree_path));
        }

        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(worktree_path.to_str().unwrap());

        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(&args)
            .output()?;

        if !output.status.success() {
            return Err(WorktreeError::GitError(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(())
    }

    /// Force cleanup a task-based worktree: remove directory, prune refs, delete branch.
    pub fn force_cleanup_task_worktree(&self, worktree_id: &str) -> WorktreeResult<()> {
        let worktree_path = self.task_worktree_path(worktree_id);

        // Get the branch name before removal
        let branch_name = if worktree_path.exists() {
            Command::new("git")
                .current_dir(&worktree_path)
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|b| b != "HEAD")
        } else {
            None
        };

        // Try git removal first
        let _ = self.remove_task_worktree(worktree_id, true);

        // If directory still exists, remove manually
        if worktree_path.exists() {
            std::fs::remove_dir_all(&worktree_path)?;
        }

        self.prune()?;

        // Delete the branch
        if let Some(branch) = &branch_name {
            let _ = Command::new("git")
                .current_dir(&self.repo_root)
                .args(["branch", "-D", branch])
                .output();
        }

        Ok(())
    }

    /// Get the base path for task-based worktrees.
    pub fn task_worktrees_base(&self) -> &Path {
        &self.task_worktrees_base
    }

    /// Find orphaned task-based worktrees — directories in the task worktrees
    /// base that are not tracked in the registry.
    pub fn find_orphaned_task_worktrees(&self, registered_ids: &[String]) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.task_worktrees_base) else {
            return vec![];
        };

        entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .filter(|name| !registered_ids.contains(name))
            .collect()
    }

    /// Clean up stale branches that match the task-based naming pattern
    /// (task-<id>-* or review-pr-*) and are already merged into the default branch.
    ///
    /// Uses `git branch --merged` for a single-call check of all merged branches,
    /// then batches deletions into one `git branch -D` call. This avoids the
    /// O(N) subprocess calls that previously blocked the daemon event loop
    /// for minutes when thousands of branches existed.
    pub fn clean_stale_task_branches(&self) -> Vec<String> {
        let default_branch =
            detect_default_branch(&self.repo_root).unwrap_or_else(|| "main".to_string());

        // Single call: get all branches already merged into the default branch
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args([
                "branch",
                "--merged",
                &default_branch,
                "--format=%(refname:short)",
            ])
            .output();

        let merged_branches: Vec<String> = match output {
            Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect(),
            _ => return vec![],
        };

        // Get branches in use by worktrees (via git worktree list --porcelain)
        let worktree_branches: std::collections::HashSet<String> = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|line| line.strip_prefix("branch refs/heads/"))
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        // Filter to task/review branches not in use by worktrees
        let to_delete: Vec<String> = merged_branches
            .into_iter()
            .filter(|branch| {
                branch != &default_branch
                    && (branch.starts_with("task-") || branch.starts_with("review-pr-"))
                    && !worktree_branches.contains(branch)
            })
            .collect();

        if to_delete.is_empty() {
            return vec![];
        }

        // Single batched deletion: `git branch -D branch1 branch2 ...`
        let mut args = vec!["branch", "-D"];
        args.extend(to_delete.iter().map(|s| s.as_str()));
        let result = Command::new("git")
            .current_dir(&self.repo_root)
            .args(&args)
            .output();

        match result {
            Ok(output) if output.status.success() => to_delete,
            Ok(output) => {
                // Partial failure: some branches may have been deleted.
                // Parse stderr to find which ones failed and return the rest.
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::debug!("Partial branch cleanup: {}", stderr);
                // On partial failure, conservatively return empty — the branches
                // that were deleted will be absent on the next run.
                vec![]
            }
            Err(_) => vec![],
        }
    }
}

/// Normalize a path for stable matching across canonical and non-canonical forms.
///
/// On macOS temp directories are often referenced as both `/var/...` and
/// `/private/var/...`. Git worktree commands usually emit canonicalized
/// `/private/...` paths, while config-derived paths may use `/var/...`.
fn normalize_path_for_matching(path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let normalized = canonical.components().collect::<PathBuf>();

    #[cfg(unix)]
    {
        let s = normalized.to_string_lossy();
        if s == "/private/var" {
            return PathBuf::from("/var");
        }
        if let Some(rest) = s.strip_prefix("/private/var/") {
            return PathBuf::from("/var").join(rest);
        }
    }

    normalized
}

/// Return true when two paths refer to the same normalized location.
fn paths_match(left: &Path, right: &Path) -> bool {
    normalize_path_for_matching(left) == normalize_path_for_matching(right)
}

/// Detect the git repository root from the current directory.
fn detect_repo_root() -> WorktreeResult<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| WorktreeError::RepoDetection(e.to_string()))?;

    if !output.status.success() {
        return Err(WorktreeError::RepoDetection(
            "Not in a git repository".to_string(),
        ));
    }

    let path_str = String::from_utf8_lossy(&output.stdout);
    Ok(PathBuf::from(path_str.trim()))
}

/// Detect the repository name from a path using git.
///
/// Uses `git rev-parse --git-common-dir` to correctly identify the main repo
/// name even when `repo_path` is inside a git worktree. Falls back to
/// extracting the last path component if git detection fails.
fn repo_name_from_path(repo_path: &Path) -> WorktreeResult<String> {
    // Try git-aware detection first (handles worktree paths correctly)
    let git_result = Command::new("git")
        .current_dir(repo_path)
        .args(["rev-parse", "--git-common-dir"])
        .output();

    match git_result {
        Ok(output) if output.status.success() => {
            let git_common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();

            if git_common_dir == ".git" {
                // Regular repo (not a worktree) — use --show-toplevel to resolve
                let tl_result = Command::new("git")
                    .current_dir(repo_path)
                    .args(["rev-parse", "--show-toplevel"])
                    .output();

                match tl_result {
                    Ok(tl) if tl.status.success() => {
                        let path_str = String::from_utf8_lossy(&tl.stdout);
                        if let Some(name) = Path::new(path_str.trim())
                            .file_name()
                            .and_then(|s| s.to_str())
                        {
                            return Ok(name.to_string());
                        }
                        warn!(
                            "git rev-parse --show-toplevel returned unparseable path '{}' for {}, falling back to path extraction",
                            path_str.trim(),
                            repo_path.display()
                        );
                    }
                    Ok(tl) => {
                        warn!(
                            "git rev-parse --show-toplevel failed for {} (status {}), falling back to path extraction: {}",
                            repo_path.display(),
                            tl.status,
                            String::from_utf8_lossy(&tl.stderr).trim()
                        );
                    }
                    Err(e) => {
                        warn!(
                            "git rev-parse --show-toplevel failed to execute for {}: {}, falling back to path extraction",
                            repo_path.display(),
                            e
                        );
                    }
                }
            } else {
                // Worktree: git-common-dir is the main repo's .git directory
                let git_path = Path::new(&git_common_dir);
                if let Some(name) = git_path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                {
                    return Ok(name.to_string());
                }
                warn!(
                    "Could not extract repo name from git-common-dir '{}' for {}, falling back to path extraction",
                    git_common_dir,
                    repo_path.display()
                );
            }
        }
        Ok(output) => {
            warn!(
                "git rev-parse --git-common-dir failed for {} (status {}), falling back to path extraction: {}",
                repo_path.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Err(e) => {
            warn!(
                "git rev-parse --git-common-dir failed to execute for {}: {}, falling back to path extraction",
                repo_path.display(),
                e
            );
        }
    }

    // Fallback: simple path extraction
    repo_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| WorktreeError::RepoDetection("Could not determine repo name".to_string()))
}

/// Detect the default branch (main or master) for a repository.
pub fn detect_default_branch(repo_root: &Path) -> Option<String> {
    // Try origin/HEAD first
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()
        .ok()?;

    if output.status.success() {
        let refname = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return refname
            .strip_prefix("refs/remotes/origin/")
            .map(|s| s.to_string());
    }

    // Fallback: check if "main" or "master" branch exists
    for branch in &["main", "master"] {
        let output = Command::new("git")
            .current_dir(repo_root)
            .args(["rev-parse", "--verify", &format!("refs/heads/{}", branch)])
            .output();
        if let Ok(o) = output
            && o.status.success()
        {
            return Some(branch.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_worktree_path() {
        let manager = WorktreeManager {
            repo_root: PathBuf::from("/tmp/repo"),
            repo_name: "myrepo".to_string(),
            task_worktrees_base: PathBuf::from("/home/user/.midtown/projects/myrepo/worktrees"),
        };

        assert_eq!(
            manager.task_worktree_path("task-42-add-auth"),
            PathBuf::from("/home/user/.midtown/projects/myrepo/worktrees/task-42-add-auth")
        );
    }

    use std::process::Command as TestCommand;
    use tempfile::TempDir;

    /// Create a temp git repo with an initial commit
    fn create_test_repo() -> (WorktreeManager, TempDir) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        TestCommand::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git init");
        TestCommand::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config email");
        TestCommand::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config name");
        TestCommand::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial commit"])
            .current_dir(temp_dir.path())
            .output()
            .expect("initial commit");

        let manager =
            WorktreeManager::new(temp_dir.path().to_path_buf()).expect("create worktree manager");
        (manager, temp_dir)
    }

    #[test]
    fn test_detect_default_branch() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        TestCommand::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git init");
        TestCommand::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config email");
        TestCommand::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config name");
        TestCommand::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial"])
            .current_dir(temp_dir.path())
            .output()
            .expect("initial commit");

        // Should detect whatever default branch was created (usually "main" on modern git)
        let default = detect_default_branch(temp_dir.path());
        assert!(
            default.is_some(),
            "Should detect a default branch (main or master)"
        );
    }

    #[test]
    fn test_new_with_worktree_path_detects_correct_repo_name() {
        let temp_dir = TempDir::new().expect("create temp dir");

        // Create the main repo
        TestCommand::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git init");
        TestCommand::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config email");
        TestCommand::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config name");
        TestCommand::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial commit"])
            .current_dir(temp_dir.path())
            .output()
            .expect("initial commit");

        // Create worktree inside a second TempDir (ensures cleanup on panic)
        let worktree_parent = TempDir::new().expect("create worktree parent dir");
        let worktree_dir = worktree_parent.path().join("task-42-some-feature");
        let wt_output = TestCommand::new("git")
            .args([
                "worktree",
                "add",
                worktree_dir.to_str().unwrap(),
                "-b",
                "task-42",
            ])
            .current_dir(temp_dir.path())
            .output()
            .expect("git worktree add");
        assert!(
            wt_output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&wt_output.stderr)
        );

        // Create a WorktreeManager using the WORKTREE path (simulates the bug)
        let wt_manager =
            WorktreeManager::new(worktree_dir.clone()).expect("create manager from worktree path");

        // The repo_name should be the main repo's name, not "task-42-some-feature"
        let expected_name = temp_dir.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(
            wt_manager.repo_name(),
            expected_name,
            "repo_name should be the main repo name '{}', not the worktree dir name '{}'",
            expected_name,
            wt_manager.repo_name()
        );
    }

    #[test]
    fn test_fallback_when_not_in_git_repo() {
        // Create a temp dir that is NOT a git repo
        let temp_dir = TempDir::new().expect("create temp dir");
        let subdir = temp_dir.path().join("myproject");
        std::fs::create_dir(&subdir).expect("create subdir");

        // WorktreeManager::new should fall back to path extraction
        let manager =
            WorktreeManager::new(subdir.clone()).expect("create manager from non-git path");

        // Should use the directory name as repo name
        assert_eq!(manager.repo_name(), "myproject");
    }

    #[test]
    fn test_regular_repo_detects_correct_name() {
        // Test the regular repo case (git-common-dir == ".git")
        let temp_dir = TempDir::new().expect("create temp dir");

        // Create a regular git repo
        TestCommand::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git init");
        TestCommand::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config email");
        TestCommand::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config name");
        TestCommand::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial commit"])
            .current_dir(temp_dir.path())
            .output()
            .expect("initial commit");

        let manager = WorktreeManager::new(temp_dir.path().to_path_buf()).expect("create manager");

        let expected_name = temp_dir.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(
            manager.repo_name(),
            expected_name,
            "regular repo should detect correct name"
        );
    }

    #[test]
    fn test_create_lead_worktree() {
        let (manager, _temp_dir) = create_test_repo();

        // First call creates the worktree
        let path = manager
            .create_lead_worktree()
            .expect("create lead worktree");
        assert!(path.exists());
        assert!(path.ends_with("lead"));
        assert!(manager.is_worktree_registered(&path));

        // Second call is idempotent — returns same path
        let path2 = manager
            .create_lead_worktree()
            .expect("create lead worktree again");
        assert_eq!(path, path2);
    }

    #[test]
    fn test_create_lead_worktree_is_detached() {
        let (manager, _temp_dir) = create_test_repo();
        let path = manager
            .create_lead_worktree()
            .expect("create lead worktree");

        // Verify it's in detached HEAD state (not on a branch)
        let output = TestCommand::new("git")
            .current_dir(&path)
            .args(["symbolic-ref", "HEAD"])
            .output()
            .expect("git symbolic-ref");
        assert!(
            !output.status.success(),
            "Lead worktree should be in detached HEAD, not on a branch"
        );
    }

    #[test]
    fn test_create_lead_worktree_updates_to_head() {
        let (manager, _temp_dir) = create_test_repo();

        // Create the lead worktree at the initial commit
        let path = manager
            .create_lead_worktree()
            .expect("create lead worktree");
        let initial_head = git_head_commit(manager.repo_root());
        assert_eq!(git_head_commit(&path), initial_head);

        // Advance HEAD in the main repo
        TestCommand::new("git")
            .args(["commit", "--allow-empty", "-m", "Second commit"])
            .current_dir(manager.repo_root())
            .output()
            .expect("second commit");
        let new_head = git_head_commit(manager.repo_root());
        assert_ne!(initial_head, new_head, "HEAD should have advanced");

        // Worktree is still at the old commit
        assert_eq!(git_head_commit(&path), initial_head);

        // Calling create_lead_worktree again should update it
        let path2 = manager
            .create_lead_worktree()
            .expect("update lead worktree");
        assert_eq!(path, path2);
        assert_eq!(
            git_head_commit(&path),
            new_head,
            "Lead worktree should now be at the new HEAD"
        );
    }

    #[test]
    fn test_create_lead_worktree_noop_when_already_at_head() {
        let (manager, _temp_dir) = create_test_repo();

        let path = manager
            .create_lead_worktree()
            .expect("create lead worktree");
        let head = git_head_commit(manager.repo_root());

        // Calling again with same HEAD should succeed without error
        let path2 = manager
            .create_lead_worktree()
            .expect("update lead worktree (noop)");
        assert_eq!(path, path2);
        assert_eq!(git_head_commit(&path), head);
    }

    fn git_head_commit(dir: &Path) -> String {
        let output = TestCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("git rev-parse HEAD");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

#[path = "worktree_stale_branch_tests.rs"]
#[cfg(test)]
mod stale_branch_tests;
