//! Git worktree management for coworker isolation.
//!
//! Each coworker gets an isolated git worktree at:
//! `~/.midtown/<repo>/worktrees/<coworker-name>/`
//!
//! The worktree is created with a dedicated branch `<coworker-name>/work`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Error;

/// Result of worktree operations
pub type WorktreeResult<T> = std::result::Result<T, WorktreeError>;

/// Errors specific to worktree operations
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("Failed to detect repository: {0}")]
    RepoDetection(String),

    #[error("Worktree already exists: {0}")]
    AlreadyExists(PathBuf),

    #[error("Worktree does not exist: {0}")]
    NotFound(PathBuf),

    #[error("Git command failed: {0}")]
    GitError(String),

    #[error("Branch {0} exists but is not merged to main")]
    UnmergedBranch(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<WorktreeError> for Error {
    fn from(e: WorktreeError) -> Self {
        Error::Io(std::io::Error::other(e.to_string()))
    }
}

/// Manages git worktrees for coworker isolation
#[derive(Debug)]
pub struct WorktreeManager {
    /// Root repository path (the main checkout)
    repo_root: PathBuf,
    /// Repository name (for ~/.midtown/<repo>/)
    repo_name: String,
    /// Base path for worktrees (~/.midtown/<repo>/worktrees/)
    worktrees_base: PathBuf,
}

impl WorktreeManager {
    /// Create a new worktree manager by detecting the repository from the current directory.
    pub fn from_current_dir() -> WorktreeResult<Self> {
        let repo_root = detect_repo_root()?;
        let repo_name = repo_name_from_path(&repo_root)?;
        let worktrees_base = worktrees_base_path(&repo_name)?;

        Ok(Self {
            repo_root,
            repo_name,
            worktrees_base,
        })
    }

    /// Create a new worktree manager for a specific repository.
    pub fn new(repo_root: PathBuf) -> WorktreeResult<Self> {
        let repo_name = repo_name_from_path(&repo_root)?;
        let worktrees_base = worktrees_base_path(&repo_name)?;

        Ok(Self {
            repo_root,
            repo_name,
            worktrees_base,
        })
    }

    /// Get the worktree path for a coworker
    pub fn worktree_path(&self, coworker_name: &str) -> PathBuf {
        self.worktrees_base.join(coworker_name)
    }

    /// Create a worktree for a coworker.
    ///
    /// Creates a new worktree at `~/.midtown/<repo>/worktrees/<name>/`
    /// detached at the current HEAD. The coworker should immediately create
    /// a feature branch for their task.
    pub fn create(&self, coworker_name: &str) -> WorktreeResult<PathBuf> {
        let worktree_path = self.worktree_path(coworker_name);

        // Check if worktree already exists
        if worktree_path.exists() {
            return Err(WorktreeError::AlreadyExists(worktree_path));
        }

        // Ensure the base directory exists
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Create the worktree detached at HEAD
        // Coworker will create their own feature branch for each task
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
            return Err(WorktreeError::GitError(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(worktree_path)
    }

    /// Remove a coworker's worktree.
    ///
    /// If `force` is true, removes the worktree even if it has uncommitted changes.
    pub fn remove(&self, coworker_name: &str, force: bool) -> WorktreeResult<()> {
        let worktree_path = self.worktree_path(coworker_name);

        // Check if worktree exists
        if !worktree_path.exists() {
            return Err(WorktreeError::NotFound(worktree_path));
        }

        // Remove the worktree
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

    /// Force remove a worktree directory and prune git references.
    ///
    /// This is useful for cleaning up stale worktrees that weren't properly
    /// removed (e.g., after a crash or forced shutdown).
    pub fn force_cleanup(&self, coworker_name: &str) -> WorktreeResult<()> {
        let worktree_path = self.worktree_path(coworker_name);

        // Try to remove via git first (handles lock files, etc.)
        let _ = self.remove(coworker_name, true);

        // If the directory still exists, remove it manually
        if worktree_path.exists() {
            std::fs::remove_dir_all(&worktree_path)?;
        }

        // Prune any stale git worktree references
        self.prune()?;

        Ok(())
    }

    /// List all worktrees for this repository.
    pub fn list(&self) -> WorktreeResult<Vec<WorktreeInfo>> {
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["worktree", "list", "--porcelain"])
            .output()?;

        if !output.status.success() {
            return Err(WorktreeError::GitError(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let worktrees = parse_worktree_list(&stdout, &self.worktrees_base);

        Ok(worktrees)
    }

    /// Get the repository root path.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Get the repository name.
    pub fn repo_name(&self) -> &str {
        &self.repo_name
    }
}

/// Information about a worktree
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    /// Path to the worktree
    pub path: PathBuf,
    /// Branch checked out in this worktree (if any)
    pub branch: Option<String>,
    /// Whether this is a coworker worktree (under our management)
    pub is_coworker: bool,
    /// Coworker name (extracted from path if is_coworker)
    pub coworker_name: Option<String>,
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

/// Extract repository name from the repository path.
fn repo_name_from_path(repo_path: &Path) -> WorktreeResult<String> {
    repo_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| WorktreeError::RepoDetection("Could not determine repo name".to_string()))
}

/// Get the base path for worktrees (~/.midtown/<repo>/worktrees/).
fn worktrees_base_path(repo_name: &str) -> WorktreeResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        WorktreeError::RepoDetection("Could not determine home directory".to_string())
    })?;

    Ok(home.join(".midtown").join(repo_name).join("worktrees"))
}

/// Parse the output of `git worktree list --porcelain`.
fn parse_worktree_list(output: &str, worktrees_base: &Path) -> Vec<WorktreeInfo> {
    let mut worktrees = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;

    for line in output.lines() {
        if line.starts_with("worktree ") {
            // Save previous worktree if any
            if let Some(path) = current_path.take() {
                let (is_coworker, coworker_name) = check_coworker_worktree(&path, worktrees_base);
                worktrees.push(WorktreeInfo {
                    path,
                    branch: current_branch.take(),
                    is_coworker,
                    coworker_name,
                });
            }
            current_path = Some(PathBuf::from(line.strip_prefix("worktree ").unwrap()));
        } else if line.starts_with("branch ") {
            let branch = line
                .strip_prefix("branch refs/heads/")
                .unwrap_or(line.strip_prefix("branch ").unwrap_or(""));
            current_branch = Some(branch.to_string());
        }
    }

    // Don't forget the last worktree
    if let Some(path) = current_path {
        let (is_coworker, coworker_name) = check_coworker_worktree(&path, worktrees_base);
        worktrees.push(WorktreeInfo {
            path,
            branch: current_branch,
            is_coworker,
            coworker_name,
        });
    }

    worktrees
}

/// Check if a worktree path is under our management and extract coworker name.
fn check_coworker_worktree(path: &Path, worktrees_base: &Path) -> (bool, Option<String>) {
    if path.starts_with(worktrees_base) {
        let relative = path.strip_prefix(worktrees_base).ok();
        let coworker_name = relative
            .and_then(|p| p.components().next())
            .and_then(|c| c.as_os_str().to_str())
            .map(|s| s.to_string());
        (coworker_name.is_some(), coworker_name)
    } else {
        (false, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worktree_path() {
        let manager = WorktreeManager {
            repo_root: PathBuf::from("/tmp/repo"),
            repo_name: "myrepo".to_string(),
            worktrees_base: PathBuf::from("/home/user/.midtown/myrepo/worktrees"),
        };

        assert_eq!(
            manager.worktree_path("alice"),
            PathBuf::from("/home/user/.midtown/myrepo/worktrees/alice")
        );
    }

    #[test]
    fn test_parse_worktree_list() {
        let output = r#"worktree /home/user/repo
HEAD abc123
branch refs/heads/main

worktree /home/user/.midtown/myrepo/worktrees/alice
HEAD def456
branch refs/heads/alice/work

worktree /home/user/.midtown/myrepo/worktrees/bob
HEAD 789xyz
branch refs/heads/bob/work
"#;

        let base = PathBuf::from("/home/user/.midtown/myrepo/worktrees");
        let worktrees = parse_worktree_list(output, &base);

        assert_eq!(worktrees.len(), 3);

        // Main worktree
        assert_eq!(worktrees[0].path, PathBuf::from("/home/user/repo"));
        assert_eq!(worktrees[0].branch, Some("main".to_string()));
        assert!(!worktrees[0].is_coworker);
        assert!(worktrees[0].coworker_name.is_none());

        // Alice's worktree
        assert_eq!(
            worktrees[1].path,
            PathBuf::from("/home/user/.midtown/myrepo/worktrees/alice")
        );
        assert_eq!(worktrees[1].branch, Some("alice/work".to_string()));
        assert!(worktrees[1].is_coworker);
        assert_eq!(worktrees[1].coworker_name, Some("alice".to_string()));

        // Bob's worktree
        assert!(worktrees[2].is_coworker);
        assert_eq!(worktrees[2].coworker_name, Some("bob".to_string()));
    }

    #[test]
    fn test_check_coworker_worktree() {
        let base = PathBuf::from("/home/user/.midtown/myrepo/worktrees");

        let (is_coworker, name) = check_coworker_worktree(
            &PathBuf::from("/home/user/.midtown/myrepo/worktrees/alice"),
            &base,
        );
        assert!(is_coworker);
        assert_eq!(name, Some("alice".to_string()));

        let (is_coworker, name) =
            check_coworker_worktree(&PathBuf::from("/home/user/other/repo"), &base);
        assert!(!is_coworker);
        assert!(name.is_none());
    }
}
