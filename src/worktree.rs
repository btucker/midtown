//! Git worktree management for coworker isolation.
//!
//! Each coworker gets an isolated git worktree at:
//! `~/.midtown/coworkers/<repo>/<coworker-name>/`
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

    #[error("Branch {0} exists but is not merged to the default branch")]
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
    /// Repository name (for ~/.midtown/coworkers/<repo>/)
    repo_name: String,
    /// Base path for worktrees (~/.midtown/coworkers/<repo>/)
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
    /// Creates a new worktree at `~/.midtown/coworkers/<repo>/<name>/`
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
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            // If the directory was deleted externally but git still tracks the
            // worktree, prune stale references and retry once.
            if !worktree_path.exists() {
                tracing::warn!(
                    "git worktree add failed ({}), pruning stale references and retrying",
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

                return Ok(worktree_path);
            }

            return Err(WorktreeError::GitError(stderr));
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

    /// Get the branch name checked out in a coworker's worktree.
    ///
    /// Returns None if the worktree doesn't exist or is in detached HEAD state.
    pub fn get_branch(&self, coworker_name: &str) -> Option<String> {
        let worktree_path = self.worktree_path(coworker_name);
        if !worktree_path.exists() {
            return None;
        }

        let branch_output = Command::new("git")
            .current_dir(&worktree_path)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output();

        match branch_output {
            Ok(output) if output.status.success() => {
                let b = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if b == "HEAD" {
                    None // Detached HEAD
                } else {
                    Some(b)
                }
            }
            _ => None,
        }
    }

    /// Check if a coworker's worktree is on the default branch (main/master).
    ///
    /// This is used to prevent coworkers from working on the default branch,
    /// which can cause conflicts with other worktrees and accidental commits to main.
    ///
    /// Returns `true` if the worktree is checked out on the default branch.
    /// Returns `false` if the worktree doesn't exist, is detached, or is on a feature branch.
    pub fn is_on_default_branch(&self, coworker_name: &str) -> bool {
        let Some(branch) = self.get_branch(coworker_name) else {
            return false;
        };

        let default_branch =
            detect_default_branch(&self.repo_root).unwrap_or_else(|| "main".to_string());
        branch == default_branch
    }

    /// Create a new feature branch in a coworker's worktree.
    ///
    /// This is used to recover from situations where a coworker's worktree is on
    /// the default branch. The branch name follows the pattern `<coworker>/<suffix>`.
    ///
    /// Returns the name of the created branch, or an error if the operation fails.
    pub fn checkout_new_branch(
        &self,
        coworker_name: &str,
        branch_suffix: &str,
    ) -> WorktreeResult<String> {
        let worktree_path = self.worktree_path(coworker_name);
        if !worktree_path.exists() {
            return Err(WorktreeError::NotFound(worktree_path));
        }

        let branch_name = format!("{}/{}", coworker_name, branch_suffix);

        let output = Command::new("git")
            .current_dir(&worktree_path)
            .args(["checkout", "-b", &branch_name])
            .output()?;

        if !output.status.success() {
            return Err(WorktreeError::GitError(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(branch_name)
    }

    /// Check if a coworker's branch has commits not on the default branch.
    ///
    /// Returns `true` if the branch has unique commits that are not on the default branch.
    /// Returns `false` if the branch has no commits beyond the base, or if the
    /// worktree is in detached HEAD state with no branch.
    pub fn has_commits_beyond_base(&self, coworker_name: &str) -> bool {
        let worktree_path = self.worktree_path(coworker_name);
        if !worktree_path.exists() {
            return false;
        }

        // Get the current branch in the worktree
        let branch_output = Command::new("git")
            .current_dir(&worktree_path)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output();

        let branch = match branch_output {
            Ok(output) if output.status.success() => {
                let b = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if b == "HEAD" {
                    // Detached HEAD - no branch, no commits to check
                    return false;
                }
                b
            }
            _ => return false,
        };

        // Get the default branch name (main or master)
        let default_branch = detect_default_branch(&self.repo_root).unwrap_or("main".to_string());

        // Count commits on this branch that aren't on the default branch
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args([
                "rev-list",
                "--count",
                &format!("{}..{}", default_branch, branch),
            ])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let count_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                count_str.parse::<u64>().unwrap_or(0) > 0
            }
            _ => {
                // If we can't determine, assume it has commits (safe default)
                true
            }
        }
    }

    /// Check if a coworker's worktree has uncommitted or staged changes.
    ///
    /// Returns `true` if the working tree is dirty (has modifications, staged
    /// changes, or untracked files). This prevents data loss when cleaning up
    /// worktrees that have work-in-progress that hasn't been committed yet.
    pub fn has_uncommitted_changes(&self, coworker_name: &str) -> bool {
        let worktree_path = self.worktree_path(coworker_name);
        if !worktree_path.exists() {
            return false;
        }

        // git status --porcelain returns empty output for a clean tree
        let output = Command::new("git")
            .current_dir(&worktree_path)
            .args(["status", "--porcelain"])
            .output();

        match output {
            Ok(output) if output.status.success() => !output.stdout.is_empty(),
            _ => {
                // If we can't determine, assume dirty (safe default)
                true
            }
        }
    }

    /// Check if a coworker's branch has a merged PR on GitHub.
    ///
    /// Uses `gh pr list` to check if the branch's PR was merged (e.g. via
    /// squash-merge). This catches cases where `has_commits_beyond_base`
    /// returns a false positive because squash-merged commits have different
    /// SHAs than the branch commits.
    pub fn is_branch_pr_merged(&self, coworker_name: &str) -> bool {
        let worktree_path = self.worktree_path(coworker_name);
        if !worktree_path.exists() {
            return false;
        }

        // Get the current branch in the worktree
        let branch_output = Command::new("git")
            .current_dir(&worktree_path)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output();

        let branch = match branch_output {
            Ok(output) if output.status.success() => {
                let b = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if b == "HEAD" {
                    return false; // Detached HEAD, can't check PR
                }
                b
            }
            _ => return false,
        };

        // Check if there's a merged PR for this branch
        let output = Command::new("gh")
            .current_dir(&self.repo_root)
            .args([
                "pr", "list", "--head", &branch, "--state", "merged", "--json", "number",
                "--limit", "1",
            ])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                // gh returns "[]" when no merged PRs found, non-empty array otherwise
                stdout != "[]" && !stdout.is_empty()
            }
            _ => false, // If gh fails, assume not merged (safe default)
        }
    }

    /// Safely clean up a coworker's worktree and branch if it has no commits
    /// and no uncommitted changes, or if the branch's PR has been merged.
    ///
    /// Returns `Ok(true)` if the worktree was cleaned up.
    /// Returns `Ok(false)` if the worktree has commits or uncommitted changes
    /// and was left intact.
    /// Returns `Err` if the cleanup operation failed.
    /// Safely clean up a coworker's worktree and branch if it has no commits
    /// and no uncommitted changes.
    ///
    /// Returns `Ok(true)` if the worktree was cleaned up.
    /// Returns `Ok(false)` if the worktree has commits or uncommitted changes.
    /// Returns `Err` if the cleanup operation failed.
    ///
    /// NOTE: This does NOT check if the branch's PR was merged (squash-merge case).
    /// That check is done at the dispatch layer using the cached PR data, to avoid
    /// expensive gh CLI calls on every tick.
    pub fn safe_cleanup(&self, coworker_name: &str) -> WorktreeResult<bool> {
        let worktree_path = self.worktree_path(coworker_name);
        if !worktree_path.exists() {
            return Ok(true); // Already gone
        }

        // If the branch has commits beyond base, don't delete automatically.
        // The dispatch layer will use cached PR data to determine if this is
        // actually safe to clean up (e.g., squash-merged PR).
        if self.has_commits_beyond_base(coworker_name) {
            return Ok(false);
        }

        if self.has_uncommitted_changes(coworker_name) {
            return Ok(false); // Has uncommitted work, don't delete
        }

        // Get the branch name before removing the worktree
        let branch_name = Command::new("git")
            .current_dir(&worktree_path)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|b| b != "HEAD");

        // Remove the worktree
        self.force_cleanup(coworker_name)?;

        // Delete the branch if it was a named branch (not detached HEAD)
        if let Some(branch) = branch_name {
            let _ = Command::new("git")
                .current_dir(&self.repo_root)
                .args(["branch", "-D", &branch])
                .output();
        }

        Ok(true)
    }

    /// Find orphaned worktrees - worktrees that exist on disk but have no
    /// corresponding active coworker.
    ///
    /// Returns a list of coworker names whose worktrees are orphaned.
    pub fn find_orphaned_worktrees(&self, active_coworkers: &[String]) -> Vec<String> {
        let worktrees = match self.list() {
            Ok(wt) => wt,
            Err(_) => return vec![],
        };

        worktrees
            .into_iter()
            .filter(|wt| wt.is_coworker)
            .filter_map(|wt| wt.coworker_name)
            .filter(|name| !active_coworkers.contains(name))
            .collect()
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

/// Get the base path for worktrees (~/.midtown/coworkers/<repo>/).
fn worktrees_base_path(repo_name: &str) -> WorktreeResult<PathBuf> {
    Ok(crate::paths::coworkers_dir_for_repo(repo_name))
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
            worktrees_base: PathBuf::from("/home/user/.midtown/coworkers/myrepo"),
        };

        assert_eq!(
            manager.worktree_path("alice"),
            PathBuf::from("/home/user/.midtown/coworkers/myrepo/alice")
        );
    }

    #[test]
    fn test_parse_worktree_list() {
        let output = r#"worktree /home/user/repo
HEAD abc123
branch refs/heads/main

worktree /home/user/.midtown/coworkers/myrepo/alice
HEAD def456
branch refs/heads/alice/work

worktree /home/user/.midtown/coworkers/myrepo/bob
HEAD 789xyz
branch refs/heads/bob/work
"#;

        let base = PathBuf::from("/home/user/.midtown/coworkers/myrepo");
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
            PathBuf::from("/home/user/.midtown/coworkers/myrepo/alice")
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
        let base = PathBuf::from("/home/user/.midtown/coworkers/myrepo");

        let (is_coworker, name) = check_coworker_worktree(
            &PathBuf::from("/home/user/.midtown/coworkers/myrepo/alice"),
            &base,
        );
        assert!(is_coworker);
        assert_eq!(name, Some("alice".to_string()));

        let (is_coworker, name) =
            check_coworker_worktree(&PathBuf::from("/home/user/other/repo"), &base);
        assert!(!is_coworker);
        assert!(name.is_none());
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
    fn test_has_commits_beyond_base_detached_head() {
        let (manager, _temp_dir) = create_test_repo();

        // Create a worktree (detached HEAD, no branch)
        let _path = manager.create("testworker").expect("create worktree");

        // Detached HEAD has no commits beyond base
        assert!(!manager.has_commits_beyond_base("testworker"));
    }

    #[test]
    fn test_has_commits_beyond_base_with_branch_no_commits() {
        let (manager, _temp_dir) = create_test_repo();

        // Create a worktree
        let wt_path = manager.create("testworker").expect("create worktree");

        // Create a branch in the worktree (same commit as main)
        TestCommand::new("git")
            .args(["checkout", "-b", "testworker/feature"])
            .current_dir(&wt_path)
            .output()
            .expect("create branch");

        // No extra commits
        assert!(!manager.has_commits_beyond_base("testworker"));
    }

    #[test]
    fn test_has_commits_beyond_base_with_commits() {
        let (manager, _temp_dir) = create_test_repo();

        // Create a worktree
        let wt_path = manager.create("testworker").expect("create worktree");

        // Create a branch and add a commit
        TestCommand::new("git")
            .args(["checkout", "-b", "testworker/feature"])
            .current_dir(&wt_path)
            .output()
            .expect("create branch");
        TestCommand::new("git")
            .args(["commit", "--allow-empty", "-m", "Feature work"])
            .current_dir(&wt_path)
            .output()
            .expect("add commit");

        // Now has commits beyond base
        assert!(manager.has_commits_beyond_base("testworker"));
    }

    #[test]
    fn test_safe_cleanup_empty_branch() {
        let (manager, _temp_dir) = create_test_repo();

        // Create a worktree (detached HEAD)
        let wt_path = manager.create("testworker").expect("create worktree");
        assert!(wt_path.exists());

        // Safe cleanup should succeed (no commits)
        let cleaned = manager.safe_cleanup("testworker").expect("safe cleanup");
        assert!(cleaned);
        assert!(!wt_path.exists());
    }

    #[test]
    fn test_safe_cleanup_branch_with_commits() {
        let (manager, _temp_dir) = create_test_repo();

        // Create a worktree with a branch and commit
        let wt_path = manager.create("testworker").expect("create worktree");
        TestCommand::new("git")
            .args(["checkout", "-b", "testworker/feature"])
            .current_dir(&wt_path)
            .output()
            .expect("create branch");
        TestCommand::new("git")
            .args(["commit", "--allow-empty", "-m", "Feature work"])
            .current_dir(&wt_path)
            .output()
            .expect("add commit");

        // Safe cleanup should NOT delete (has commits)
        let cleaned = manager.safe_cleanup("testworker").expect("safe cleanup");
        assert!(!cleaned);
        assert!(wt_path.exists());
    }

    #[test]
    fn test_safe_cleanup_nonexistent() {
        let (manager, _temp_dir) = create_test_repo();

        // Safe cleanup of nonexistent worktree should return true
        let cleaned = manager
            .safe_cleanup("nonexistent")
            .expect("safe cleanup nonexistent");
        assert!(cleaned);
    }

    #[test]
    fn test_find_orphaned_worktrees() {
        let (manager, _temp_dir) = create_test_repo();

        // Create two worktrees
        manager.create("worker-a").expect("create worktree a");
        manager.create("worker-b").expect("create worktree b");

        // worker-a is active, worker-b is not
        let active = vec!["worker-a".to_string()];
        let orphaned = manager.find_orphaned_worktrees(&active);

        assert_eq!(orphaned.len(), 1);
        assert!(orphaned.contains(&"worker-b".to_string()));
    }

    #[test]
    fn test_safe_cleanup_dirty_worktree() {
        let (manager, _temp_dir) = create_test_repo();

        // Create a worktree (detached HEAD, no commits beyond base)
        let wt_path = manager.create("testworker").expect("create worktree");

        // Create an uncommitted file in the worktree
        std::fs::write(wt_path.join("dirty.txt"), "uncommitted work").expect("write file");

        // has_uncommitted_changes should detect the dirty file
        assert!(manager.has_uncommitted_changes("testworker"));

        // Safe cleanup should NOT delete (has uncommitted changes)
        let cleaned = manager.safe_cleanup("testworker").expect("safe cleanup");
        assert!(!cleaned);
        assert!(wt_path.exists());
        // The file should still be there
        assert!(wt_path.join("dirty.txt").exists());
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
    fn test_create_worktree_after_directory_deleted() {
        let (manager, _temp_dir) = create_test_repo();

        // Create a worktree
        let wt_path = manager.create("testworker").expect("create worktree");
        assert!(wt_path.exists());

        // Externally delete the worktree directory (simulates OS cleanup, user rm -rf, etc.)
        std::fs::remove_dir_all(&wt_path).expect("delete worktree dir");
        assert!(!wt_path.exists());

        // Attempting to create the same worktree again should succeed
        // because create() prunes stale git refs and retries
        let result = manager.create("testworker");
        assert!(
            result.is_ok(),
            "create should succeed after directory deleted, got: {:?}",
            result.err()
        );
        assert!(wt_path.exists(), "worktree should be recreated on disk");
    }

    #[test]
    fn test_is_on_default_branch_detached() {
        let (manager, _temp_dir) = create_test_repo();

        // Create a worktree (detached HEAD by default)
        manager.create("testworker").expect("create worktree");

        // Detached HEAD should not be considered on default branch
        assert!(!manager.is_on_default_branch("testworker"));
    }

    #[test]
    fn test_is_on_default_branch_feature_branch() {
        let (manager, _temp_dir) = create_test_repo();

        // Create a worktree
        let wt_path = manager.create("testworker").expect("create worktree");

        // Create a feature branch
        TestCommand::new("git")
            .args(["checkout", "-b", "testworker/feature"])
            .current_dir(&wt_path)
            .output()
            .expect("create branch");

        // Feature branch should not be considered on default branch
        assert!(!manager.is_on_default_branch("testworker"));
    }

    #[test]
    fn test_is_on_default_branch_on_main() {
        let (manager, _temp_dir) = create_test_repo();

        // Get the default branch name (usually "main" or "master")
        let default_branch =
            detect_default_branch(manager.repo_root()).unwrap_or_else(|| "main".to_string());

        // First, move the main repo OFF the default branch so a worktree can use it.
        // This simulates the scenario where the Lead is on a feature branch.
        TestCommand::new("git")
            .args(["checkout", "-b", "lead-feature"])
            .current_dir(manager.repo_root())
            .output()
            .expect("create lead branch");

        // Create a worktree
        let wt_path = manager.create("testworker").expect("create worktree");

        // Checkout the default branch in the worktree (now possible since main repo is on feature branch)
        TestCommand::new("git")
            .args(["checkout", &default_branch])
            .current_dir(&wt_path)
            .output()
            .expect("checkout default branch");

        // Should now be on default branch
        assert!(manager.is_on_default_branch("testworker"));
    }

    #[test]
    fn test_is_on_default_branch_nonexistent() {
        let (manager, _temp_dir) = create_test_repo();

        // Non-existent worktree should return false
        assert!(!manager.is_on_default_branch("nonexistent"));
    }

    #[test]
    fn test_checkout_new_branch() {
        let (manager, _temp_dir) = create_test_repo();

        // Create a worktree
        let wt_path = manager.create("testworker").expect("create worktree");

        // Create a new branch
        let branch = manager
            .checkout_new_branch("testworker", "feature-a")
            .expect("create branch");

        assert_eq!(branch, "testworker/feature-a");

        // Verify the worktree is now on that branch
        let current = manager.get_branch("testworker");
        assert_eq!(current, Some("testworker/feature-a".to_string()));

        // Also verify via git directly
        let output = TestCommand::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&wt_path)
            .output()
            .expect("get branch");
        let actual = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(actual, "testworker/feature-a");
    }

    #[test]
    fn test_checkout_new_branch_nonexistent_worktree() {
        let (manager, _temp_dir) = create_test_repo();

        // Should fail for non-existent worktree
        let result = manager.checkout_new_branch("nonexistent", "feature");
        assert!(result.is_err());
    }

    #[test]
    fn test_checkout_new_branch_recovery_from_main() {
        let (manager, _temp_dir) = create_test_repo();

        // Get the default branch name (usually "main" or "master")
        let default_branch =
            detect_default_branch(manager.repo_root()).unwrap_or_else(|| "main".to_string());

        // First, move the main repo OFF the default branch so a worktree can use it.
        // This simulates the scenario where the Lead is on a feature branch.
        TestCommand::new("git")
            .args(["checkout", "-b", "lead-feature"])
            .current_dir(manager.repo_root())
            .output()
            .expect("create lead branch");

        // Create a worktree
        let wt_path = manager.create("testworker").expect("create worktree");

        // Checkout the default branch in the worktree
        TestCommand::new("git")
            .args(["checkout", &default_branch])
            .current_dir(&wt_path)
            .output()
            .expect("checkout default branch");

        // Verify we're on main
        assert!(manager.is_on_default_branch("testworker"));

        // Create a recovery branch to get off main
        let branch = manager
            .checkout_new_branch("testworker", "recovery")
            .expect("create recovery branch");

        assert_eq!(branch, "testworker/recovery");

        // Should no longer be on default branch
        assert!(!manager.is_on_default_branch("testworker"));
        assert_eq!(
            manager.get_branch("testworker"),
            Some("testworker/recovery".to_string())
        );
    }
}
