use chrono::Utc;

use crate::worktree_registry::{WorktreeAssignment, WorktreeRegistry};

/// Test that bind_coworker prevents binding a coworker to a worktree that's
/// already bound to another coworker (collision guard).
///
/// This test simulates the scenario where:
/// 1. Coworker A is bound to a worktree
/// 2. An attempt is made to bind Coworker B to the same worktree
/// 3. The bind should fail with an error indicating the collision
#[test]
fn test_bind_coworker_prevents_collision() {
    let mut registry = WorktreeRegistry::new();

    let assignment = WorktreeAssignment {
        worktree_id: "task-42-add-auth".to_string(),
        branch_name: "task-42-add-auth".to_string(),
        task_id: Some("42".to_string()),
        current_coworker: None,
        pr_number: None,
        created_at: Utc::now(),
        completed_at: None,
    };

    registry.assign_worktree(assignment).unwrap();

    // Bind first coworker
    registry
        .bind_coworker("task-42-add-auth", "lexington")
        .unwrap();

    // Attempt to bind second coworker to the same worktree should fail
    let result = registry.bind_coworker("task-42-add-auth", "park");
    assert!(
        result.is_err(),
        "Expected bind_coworker to fail when worktree is already bound"
    );

    // The error message should indicate the collision
    let err_msg = result.unwrap_err();
    assert!(
        err_msg.contains("lexington"),
        "Error message should mention the existing coworker: {}",
        err_msg
    );
    assert!(
        err_msg.contains("task-42-add-auth"),
        "Error message should mention the worktree: {}",
        err_msg
    );
}

/// Test that bind_coworker allows rebinding when the worktree is unbound.
#[test]
fn test_bind_coworker_allows_rebind_after_unbind() {
    let mut registry = WorktreeRegistry::new();

    let assignment = WorktreeAssignment {
        worktree_id: "task-42-add-auth".to_string(),
        branch_name: "task-42-add-auth".to_string(),
        task_id: Some("42".to_string()),
        current_coworker: None,
        pr_number: None,
        created_at: Utc::now(),
        completed_at: None,
    };

    registry.assign_worktree(assignment).unwrap();

    // Bind first coworker
    registry
        .bind_coworker("task-42-add-auth", "lexington")
        .unwrap();

    // Unbind
    registry.unbind_coworker("lexington");

    // Binding a different coworker should now succeed
    let result = registry.bind_coworker("task-42-add-auth", "park");
    assert!(
        result.is_ok(),
        "Expected bind_coworker to succeed after unbind"
    );
}

/// Test that bind_coworker allows the same coworker to rebind to the same worktree
/// (idempotent operation).
#[test]
fn test_bind_coworker_is_idempotent() {
    let mut registry = WorktreeRegistry::new();

    let assignment = WorktreeAssignment {
        worktree_id: "task-42-add-auth".to_string(),
        branch_name: "task-42-add-auth".to_string(),
        task_id: Some("42".to_string()),
        current_coworker: None,
        pr_number: None,
        created_at: Utc::now(),
        completed_at: None,
    };

    registry.assign_worktree(assignment).unwrap();

    // Bind coworker
    registry
        .bind_coworker("task-42-add-auth", "lexington")
        .unwrap();

    // Binding the same coworker again should succeed (idempotent)
    let result = registry.bind_coworker("task-42-add-auth", "lexington");
    assert!(
        result.is_ok(),
        "Expected bind_coworker to be idempotent for the same coworker"
    );
}
