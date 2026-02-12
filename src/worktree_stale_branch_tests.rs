//! Tests for worktree creation when stale branches exist.
//!
//! These tests reproduce the bug where `create_task_worktree` fails when
//! a branch already exists but is linked to a stale (deleted) worktree.

use std::process::Command as TestCommand;
use tempfile::TempDir;

use crate::worktree::WorktreeManager;

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
fn test_create_task_worktree_with_standalone_stale_branch() {
    let (manager, temp_dir) = create_test_repo();

    // This test specifically exercises the fallback error handler (lines 883-923).
    // We create a standalone branch (not linked to any worktree) so the proactive
    // check (lines 833-859) doesn't catch it.
    //
    // Scenario: A branch exists (e.g., from a previous manual git operation or
    // worktree that was force-removed), and we try to create a worktree with
    // `git worktree add -b <branch>`. This will fail with "already exists",
    // triggering the fallback handler to delete the stale branch and retry.

    let worktree_id = "review-pr-789";

    // Create a standalone branch directly (not via worktree)
    TestCommand::new("git")
        .args(["branch", worktree_id])
        .current_dir(temp_dir.path())
        .output()
        .expect("create standalone branch");

    // Verify the branch exists
    let branch_check = TestCommand::new("git")
        .args([
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", worktree_id),
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("check branch");
    assert!(
        branch_check.status.success(),
        "Standalone branch should exist before worktree creation"
    );

    // Now try to create a worktree with the same branch name.
    // The proactive check won't delete it because there's no stale worktree reference.
    // The fallback handler should kick in when `git worktree add -b` fails.
    let worktree_path = manager.task_worktree_path(worktree_id);
    let result = manager.create_task_worktree(worktree_id);

    assert!(
        result.is_ok(),
        "Should succeed by deleting stale branch in fallback handler, got: {:?}",
        result.err()
    );
    assert!(
        worktree_path.exists(),
        "Worktree should be created after fallback cleanup"
    );

    // Verify the worktree is on the expected branch
    let branch_output = TestCommand::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(&worktree_path)
        .output()
        .expect("get branch");
    let actual_branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    assert_eq!(
        actual_branch, worktree_id,
        "Worktree should be on the correct branch"
    );
}
