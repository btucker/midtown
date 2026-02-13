use tempfile::TempDir;

/// Test helper to verify that paths::lead_worktree_path() returns the correct path.
#[test]
fn test_lead_worktree_path_helper() {
    let repo_name = "test-repo";
    let lead_worktree = midtown::paths::lead_worktree_path(repo_name);

    // Should return ~/.midtown/worktrees/<repo>/lead/
    assert!(
        lead_worktree.to_string_lossy().contains("worktrees"),
        "Lead worktree path should be in worktrees directory"
    );
    assert!(
        lead_worktree.to_string_lossy().ends_with("/lead"),
        "Lead worktree path should end with /lead"
    );
    assert!(
        lead_worktree.to_string_lossy().contains(repo_name),
        "Lead worktree path should include repo name"
    );
}

/// Integration test that verifies WorktreeManager::create_lead_worktree()
/// returns a path in the worktrees directory, not the main repo.
///
/// This test validates the fix where handle_start() was passing primary_repo
/// to spawn_lead() instead of the lead worktree path.
#[test]
fn test_worktree_manager_creates_lead_in_correct_location() {
    use std::process::Command;

    // Create a temporary git repo
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path();

    // Initialize git repo
    Command::new("git")
        .args(["init"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to init git repo");

    // Create a dummy commit (needed for worktrees)
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(repo_path)
        .output()
        .unwrap();

    std::fs::write(repo_path.join("README.md"), "test").unwrap();

    Command::new("git")
        .args(["add", "."])
        .current_dir(repo_path)
        .output()
        .unwrap();

    Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .output()
        .unwrap();

    // Create WorktreeManager
    let worktree_manager =
        midtown::worktree::WorktreeManager::new(repo_path.to_path_buf()).unwrap();

    // Create lead worktree
    let lead_worktree_path = worktree_manager.create_lead_worktree().unwrap();

    // Verify it's NOT in the main repo directory
    assert_ne!(
        lead_worktree_path, repo_path,
        "Lead worktree should not be the main repo path"
    );

    // Verify it's in a worktrees subdirectory
    let lead_path_str = lead_worktree_path.to_string_lossy();
    assert!(
        lead_path_str.contains("worktrees") || lead_path_str.contains("lead"),
        "Lead worktree path should be in worktrees directory, got: {}",
        lead_path_str
    );

    // Verify it exists
    assert!(
        lead_worktree_path.exists(),
        "Lead worktree directory should exist"
    );
}
