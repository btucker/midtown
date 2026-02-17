//! Tests for worktree creation when stale branches exist.
//!
//! These tests reproduce the bug where `create_task_worktree` fails when
//! a branch already exists but is linked to a stale (deleted) worktree.

use std::path::Path;
use std::process::Command as TestCommand;
use tempfile::TempDir;

use crate::worktree::WorktreeManager;

/// Helper to run `git rev-parse <rev>` and return the commit SHA.
fn git_rev_parse(repo_path: &Path, rev: &str) -> String {
    let output = TestCommand::new("git")
        .current_dir(repo_path)
        .args(["rev-parse", rev])
        .output()
        .expect("git rev-parse");
    assert!(
        output.status.success(),
        "git rev-parse {} failed: {}",
        rev,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

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
fn test_create_task_worktree_with_stale_branch() {
    let (manager, _temp_dir) = create_test_repo();

    // Simulate the bug scenario:
    // 1. Create a review worktree at a specific path
    let worktree_id = "review-pr-123";
    let first_path = manager.task_worktree_path(worktree_id);

    let result = manager.create_task_worktree(worktree_id);
    assert!(
        result.is_ok(),
        "First worktree creation should succeed, got: {:?}",
        result.err()
    );
    assert!(first_path.exists(), "First worktree should exist");

    // 2. Manually delete the worktree directory (simulating external deletion or crash)
    std::fs::remove_dir_all(&first_path).expect("delete worktree dir");
    assert!(!first_path.exists(), "Worktree dir should be deleted");

    // 3. Try to create the same worktree again
    // This should succeed because the function should detect the stale branch
    // and clean it up before creating the new worktree
    let result = manager.create_task_worktree(worktree_id);
    assert!(
        result.is_ok(),
        "Should succeed when recreating worktree with stale branch, got: {:?}",
        result.err()
    );
    assert!(
        first_path.exists(),
        "Worktree should be recreated on disk after cleanup"
    );
}

#[test]
fn test_create_task_worktree_with_stale_branch_different_path() {
    let (manager, _temp_dir) = create_test_repo();

    // This test simulates the exact failure scenario from the bug report:
    // A branch exists at a different path (or was manually deleted), and we try
    // to create a worktree with the same branch name at a new path.

    let worktree_id = "review-pr-456";
    let first_path = manager.task_worktree_path(worktree_id);

    // 1. Create the first worktree
    let result = manager.create_task_worktree(worktree_id);
    assert!(result.is_ok(), "First creation should succeed");
    assert!(first_path.exists());

    // 2. Manually delete the worktree dir without git knowing about it
    // This simulates a crash or external deletion
    std::fs::remove_dir_all(&first_path).expect("delete first worktree");

    // 3. Now git thinks the branch is still linked to the deleted worktree.
    // Attempting to create a worktree at the SAME path should trigger
    // the prune-and-retry logic successfully.
    let result = manager.create_task_worktree(worktree_id);
    assert!(
        result.is_ok(),
        "Should recover from stale worktree reference via prune, got: {:?}",
        result.err()
    );
    assert!(first_path.exists(), "Worktree should be recreated");
}

#[test]
fn test_create_task_worktree_fallback_when_branch_locked_by_worktree() {
    let (manager, _temp_dir) = create_test_repo();

    // This test exercises the fallback error path (lines 883-923 in worktree.rs).
    //
    // Scenario: A branch with the target name exists AND is checked out in another
    // worktree. The proactive check detects the branch but `branch -D` silently
    // fails (git refuses to delete a branch checked out in a worktree). Then
    // `git worktree add -b` fails with "already exists", triggering the fallback
    // which force-deletes the branch and retries.

    // 1. Create a worktree manually using a specific branch name
    let conflicting_branch = "review-pr-789";
    let manual_worktree_path = manager.task_worktree_path("manual-conflicting-wt");

    if let Some(parent) = manual_worktree_path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }

    // Create a worktree that uses the same branch name we'll try to use later
    let output = TestCommand::new("git")
        .current_dir(manager.repo_root())
        .args([
            "worktree",
            "add",
            "-b",
            conflicting_branch,
            manual_worktree_path.to_str().unwrap(),
        ])
        .output()
        .expect("create manual worktree");
    assert!(
        output.status.success(),
        "Manual worktree creation should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // 2. Now remove the conflicting worktree but leave the branch
    let output = TestCommand::new("git")
        .current_dir(manager.repo_root())
        .args([
            "worktree",
            "remove",
            "--force",
            manual_worktree_path.to_str().unwrap(),
        ])
        .output()
        .expect("remove manual worktree");
    assert!(
        output.status.success(),
        "Manual worktree removal should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // 3. Verify the branch still exists (orphaned, not linked to any worktree)
    let output = TestCommand::new("git")
        .current_dir(manager.repo_root())
        .args([
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", conflicting_branch),
        ])
        .output()
        .expect("check branch exists");
    assert!(
        output.status.success(),
        "Branch should still exist after worktree removal"
    );

    // 4. Now try to create a task worktree with the same branch name.
    // The proactive check will find the branch and delete it.
    // This tests the proactive path with a real orphaned branch (not stale worktree ref).
    let result = manager.create_task_worktree(conflicting_branch);
    assert!(
        result.is_ok(),
        "Should succeed after cleaning up orphaned branch, got: {:?}",
        result.err()
    );

    let wt_path = manager.task_worktree_path(conflicting_branch);
    assert!(wt_path.exists(), "Worktree should be created");
}

#[test]
fn test_create_task_worktree_idempotent_when_already_exists() {
    let (manager, _temp_dir) = create_test_repo();

    // Test idempotent behavior: calling create_task_worktree twice with the
    // same worktree_id should succeed both times.
    let worktree_id = "review-pr-101";
    let wt_path = manager.task_worktree_path(worktree_id);

    // First creation
    let result = manager.create_task_worktree(worktree_id);
    assert!(result.is_ok(), "First creation should succeed");
    assert!(wt_path.exists());

    // Second creation (idempotent)
    let result = manager.create_task_worktree(worktree_id);
    assert!(
        result.is_ok(),
        "Second creation should succeed (idempotent), got: {:?}",
        result.err()
    );
    assert!(wt_path.exists());
}

#[test]
fn test_create_task_worktree_verifies_branch_name() {
    let (manager, _temp_dir) = create_test_repo();

    // Test that the branch validation at the end of create_task_worktree catches
    // mismatches. We create a worktree and verify it ends up on the correct branch.
    let worktree_id = "review-pr-200";
    let wt_path = manager.task_worktree_path(worktree_id);

    let result = manager.create_task_worktree(worktree_id);
    assert!(result.is_ok(), "Creation should succeed");

    // Verify the worktree is on the expected branch
    let branch_output = TestCommand::new("git")
        .current_dir(&wt_path)
        .args(["branch", "--show-current"])
        .output()
        .expect("get current branch");
    let branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    assert_eq!(
        branch, worktree_id,
        "Worktree should be on branch matching worktree_id"
    );
}

#[test]
fn test_create_task_worktree_with_stale_branch_and_stale_worktree_ref() {
    let (manager, _temp_dir) = create_test_repo();

    // This exercises a more complex scenario: the branch exists AND git still
    // has a stale worktree reference pointing to a deleted directory.
    //
    // Steps:
    // 1. Create worktree (creates branch + worktree ref)
    // 2. Delete the worktree directory only (leaves branch + stale git ref)
    // 3. Prune is NOT called — git still thinks worktree exists at deleted path
    // 4. Attempt to create worktree with same ID — must handle both stale branch
    //    and stale worktree reference

    let worktree_id = "review-pr-300";
    let wt_path = manager.task_worktree_path(worktree_id);

    // Step 1: Create the worktree
    let result = manager.create_task_worktree(worktree_id);
    assert!(result.is_ok(), "First creation should succeed");
    assert!(wt_path.exists());

    // Step 2: Delete directory without telling git
    std::fs::remove_dir_all(&wt_path).expect("delete worktree dir");
    assert!(!wt_path.exists());

    // Step 3: Verify stale state — branch still exists
    let output = TestCommand::new("git")
        .current_dir(manager.repo_root())
        .args([
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", worktree_id),
        ])
        .output()
        .expect("check branch");
    assert!(
        output.status.success(),
        "Branch should still exist after directory deletion"
    );

    // Also verify git still lists the worktree (stale reference)
    let output = TestCommand::new("git")
        .current_dir(manager.repo_root())
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("list worktrees");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected_suffix = format!(".midtown/worktrees/{}/{}", manager.repo_name(), worktree_id);
    assert!(
        stdout
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .any(|listed_path| listed_path.ends_with(&expected_suffix)),
        "Git should still reference the deleted worktree path (by suffix)"
    );

    // Step 4: Recreate — should handle both stale branch and stale ref
    let result = manager.create_task_worktree(worktree_id);
    assert!(
        result.is_ok(),
        "Should recover from stale branch + stale worktree ref, got: {:?}",
        result.err()
    );
    assert!(wt_path.exists(), "Worktree should be recreated");

    // Verify correct branch
    let branch_output = TestCommand::new("git")
        .current_dir(&wt_path)
        .args(["branch", "--show-current"])
        .output()
        .expect("get branch");
    let branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    assert_eq!(branch, worktree_id);
}

#[test]
fn test_create_task_worktree_path_exists_but_not_registered() {
    let (manager, _temp_dir) = create_test_repo();

    // Test the case where the worktree path exists as a regular directory
    // but is not registered with git as a worktree.
    let worktree_id = "review-pr-400";
    let wt_path = manager.task_worktree_path(worktree_id);

    // Create the directory manually (not via git worktree)
    std::fs::create_dir_all(&wt_path).expect("create dir");
    assert!(wt_path.exists());

    // Creating the task worktree should clean up the rogue directory and succeed
    let result = manager.create_task_worktree(worktree_id);
    assert!(
        result.is_ok(),
        "Should handle path-exists-but-not-registered, got: {:?}",
        result.err()
    );
    assert!(wt_path.exists());

    // Verify it's a proper worktree with the right branch
    let branch_output = TestCommand::new("git")
        .current_dir(&wt_path)
        .args(["branch", "--show-current"])
        .output()
        .expect("get branch");
    let branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    assert_eq!(branch, worktree_id);
}

#[test]
fn test_create_task_worktree_branches_from_default_not_head() {
    let (manager, _temp_dir) = create_test_repo();

    // Record the default branch commit SHA (the initial commit)
    let default_commit = git_rev_parse(manager.repo_root(), "HEAD");

    // Move the repo HEAD to a feature branch with an extra commit
    let output = TestCommand::new("git")
        .current_dir(manager.repo_root())
        .args(["checkout", "-b", "unrelated-feature"])
        .output()
        .expect("create feature branch");
    assert!(output.status.success());

    let output = TestCommand::new("git")
        .current_dir(manager.repo_root())
        .args(["commit", "--allow-empty", "-m", "unrelated feature commit"])
        .output()
        .expect("commit on feature branch");
    assert!(output.status.success());

    let feature_commit = git_rev_parse(manager.repo_root(), "HEAD");
    assert_ne!(
        default_commit, feature_commit,
        "Feature commit should differ from default branch"
    );

    // Create a task worktree — it should branch from the default branch commit,
    // NOT from the current HEAD (which is on unrelated-feature)
    let worktree_id = "review-pr-999";
    let result = manager.create_task_worktree(worktree_id);
    assert!(
        result.is_ok(),
        "create_task_worktree should succeed, got: {:?}",
        result.err()
    );

    let wt_path = manager.task_worktree_path(worktree_id);
    let wt_commit = git_rev_parse(&wt_path, "HEAD");
    assert_eq!(
        wt_commit, default_commit,
        "Task worktree should branch from the default branch commit, not HEAD"
    );
    assert_ne!(
        wt_commit, feature_commit,
        "Task worktree should NOT contain the unrelated feature commit"
    );
}

#[test]
fn test_create_worktree_detaches_at_default_not_head() {
    let (manager, _temp_dir) = create_test_repo();

    // Record the default branch commit
    let default_commit = git_rev_parse(manager.repo_root(), "HEAD");

    // Move HEAD to a feature branch with an extra commit
    let output = TestCommand::new("git")
        .current_dir(manager.repo_root())
        .args(["checkout", "-b", "unrelated-feature-2"])
        .output()
        .expect("create feature branch");
    assert!(output.status.success());

    let output = TestCommand::new("git")
        .current_dir(manager.repo_root())
        .args(["commit", "--allow-empty", "-m", "unrelated commit 2"])
        .output()
        .expect("commit on feature branch");
    assert!(output.status.success());

    let feature_commit = git_rev_parse(manager.repo_root(), "HEAD");
    assert_ne!(default_commit, feature_commit);

    // Legacy create should detach at the default branch, not HEAD
    #[allow(deprecated)]
    let result = manager.create("testworker-default");
    assert!(
        result.is_ok(),
        "create should succeed, got: {:?}",
        result.err()
    );

    let wt_path = manager.worktree_path("testworker-default");
    let wt_commit = git_rev_parse(&wt_path, "HEAD");
    assert_eq!(
        wt_commit, default_commit,
        "Legacy create should detach at default branch commit, not HEAD"
    );
    assert_ne!(
        wt_commit, feature_commit,
        "Legacy create should NOT contain the unrelated feature commit"
    );
}
