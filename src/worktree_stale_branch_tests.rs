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
