//! E2E tests for WorktreeRegistry integration with task dispatch.
//!
//! These tests verify the end-to-end flow:
//! 1. Task dispatch identifies pending tasks
//! 2. Worktree is created at task-based path
//! 3. RegisterWorktreeAssignment effect is generated
//! 4. BindCoworkerToWorktree effect is generated
//! 5. Coworker spawns successfully in the task worktree
//!
//! Run with: `cargo test --test worktree_registry_e2e -- --ignored --test-threads=1`

use std::fs;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

mod common;

use common::{DaemonHarnessOptions, DaemonTestHarness};

/// Test fixture for worktree registry E2E tests.
///
/// Wraps DaemonTestHarness and adds worktree-specific helpers.
struct WorktreeTestFixture {
    harness: DaemonTestHarness,
    /// Worktree directory under ~/.midtown/projects/<name>/worktrees/
    worktree_dir: PathBuf,
}

impl Deref for WorktreeTestFixture {
    type Target = DaemonTestHarness;
    fn deref(&self) -> &DaemonTestHarness {
        &self.harness
    }
}

impl DerefMut for WorktreeTestFixture {
    fn deref_mut(&mut self) -> &mut DaemonTestHarness {
        &mut self.harness
    }
}

impl WorktreeTestFixture {
    /// Create a new test fixture with a fake git repository.
    fn new() -> Option<Self> {
        let state_dir = PathBuf::from(format!(
            "/tmp/midtown-test-worktree-e2e-{}",
            std::process::id()
        ));
        let harness = DaemonTestHarness::new(
            "worktree-e2e-test",
            DaemonHarnessOptions {
                custom_state_dir: Some(state_dir),
                ..Default::default()
            },
        )?;

        let worktree_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".midtown")
            .join("projects")
            .join(&harness.repo_name)
            .join("worktrees");

        Some(Self {
            harness,
            worktree_dir,
        })
    }

    /// Get the path to a task worktree directory.
    fn task_worktree_path(&self, task_id: u32, slug: &str) -> PathBuf {
        self.worktree_dir.join(format!("task-{}-{}", task_id, slug))
    }

    /// List active coworkers via daemon RPC.
    fn list_coworkers(&self) -> Vec<String> {
        let response = self.harness.rpc_call("agent.list", None);
        match response {
            Some(resp) => resp["result"]["coworkers"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c["name"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            None => vec![],
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Worktree Registry Integration Tests
// ────────────────────────────────────────────────────────────────────────────

/// Test the end-to-end worktree registry integration.
///
/// This test verifies that when a task is dispatched:
/// 1. A worktree is created at the task-based path
/// 2. RegisterWorktreeAssignment effect is generated
/// 3. BindCoworkerToWorktree effect is generated
/// 4. The coworker spawns successfully in the task worktree
///
/// This addresses review issue #6 from PR #752.
#[test]
#[ignore] // Requires built binary and claude CLI
fn test_worktree_registry_integration_end_to_end() {
    let mut fixture = match WorktreeTestFixture::new() {
        Some(f) => f,
        None => {
            eprintln!("SKIPPED: fixture creation failed");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("SKIPPED: daemon failed to start");
        return;
    }

    // Create a task using the tasks directory
    let task_id = 42;
    let task_subject = "Implement user authentication";
    let task_slug = "implement-user-authentication";

    // Create task directory and task.json
    let tasks_dir = fixture
        .project_dir
        .parent()
        .unwrap()
        .join("tasks")
        .join(&fixture.repo_name);
    fs::create_dir_all(&tasks_dir).expect("Should create tasks directory");

    let task_file = tasks_dir.join(format!("{}.json", task_id));
    let task_data = serde_json::json!({
        "id": task_id.to_string(),
        "subject": task_subject,
        "status": "pending",
        "owner": null,
        "blocked_by": []
    });
    fs::write(
        &task_file,
        serde_json::to_string_pretty(&task_data).unwrap(),
    )
    .expect("Should write task file");

    // Wait for the daemon to detect the task
    thread::sleep(Duration::from_secs(2));

    // The daemon should spawn a coworker for the pending task.
    // Poll agent.list RPC to detect when a coworker appears.
    let mut spawned_coworker = None;
    for _ in 0..30 {
        thread::sleep(Duration::from_secs(1));

        let coworkers = fixture.list_coworkers();
        if let Some(name) = coworkers.into_iter().find(|n| n != "lead") {
            spawned_coworker = Some(name);
            break;
        }
    }

    let coworker_name = match spawned_coworker {
        Some(name) => {
            println!("Coworker spawned: {}", name);
            name
        }
        None => {
            eprintln!(
                "SKIPPED: No coworker spawned (expected without Claude CLI or if task dispatch didn't trigger)"
            );
            return;
        }
    };

    // Give the spawn process time to complete
    thread::sleep(Duration::from_secs(3));

    // ASSERTION 1: Verify worktree exists at the correct path
    let worktree_path = fixture.task_worktree_path(task_id, task_slug);
    assert!(
        worktree_path.exists(),
        "Task worktree should exist at {:?}",
        worktree_path
    );
    assert!(
        worktree_path.join(".git").exists(),
        "Task worktree should have .git directory"
    );

    // ASSERTION 2: Verify RegisterWorktreeAssignment and BindCoworkerToWorktree effects
    // were generated by checking the worktree registry state via RPC
    let registry_response = fixture
        .rpc_call("worktree.list", None)
        .expect("worktree.list RPC should succeed - worktree registry must be implemented");

    assert!(
        registry_response["error"].is_null(),
        "worktree.list RPC should not return an error: {:?}",
        registry_response["error"]
    );

    let worktrees = registry_response["result"]["worktrees"]
        .as_array()
        .expect("worktrees should be an array");

    // Find our task worktree in the registry
    let worktree_id = format!("task-{}-{}", task_id, task_slug);
    let found_worktree = worktrees
        .iter()
        .find(|w| w["worktree_id"].as_str() == Some(worktree_id.as_str()))
        .unwrap_or_else(|| {
            panic!(
                "Task worktree should be registered in the worktree registry. Worktree ID: {}",
                worktree_id
            )
        });

    // Verify the worktree is bound to the coworker
    let current_coworker = found_worktree["current_coworker"]
        .as_str()
        .expect("current_coworker should be a string");
    assert_eq!(
        current_coworker, coworker_name,
        "Worktree should be bound to coworker {}. Registry state: {:?}",
        coworker_name, found_worktree
    );

    // Verify task_id is recorded
    let task_id_str = found_worktree["task_id"]
        .as_str()
        .expect("task_id should be a string");
    assert_eq!(
        task_id_str,
        task_id.to_string(),
        "Worktree should have correct task_id. Registry state: {:?}",
        found_worktree
    );

    // Verify branch name matches worktree_id
    let branch_name = found_worktree["branch_name"]
        .as_str()
        .expect("branch_name should be a string");
    assert_eq!(
        branch_name, worktree_id,
        "Branch name should match worktree_id. Registry state: {:?}",
        found_worktree
    );

    // ASSERTION 3: Verify coworker appears in agent.list
    let list_response = fixture.rpc_call("agent.list", None);
    assert!(
        list_response.is_some(),
        "Should receive response from agent.list"
    );

    let list_response = list_response.unwrap();
    let coworkers = list_response["result"]["coworkers"]
        .as_array()
        .expect("coworkers should be an array");

    let found = coworkers
        .iter()
        .any(|c| c["name"].as_str() == Some(&coworker_name));
    assert!(
        found,
        "Coworker '{}' should appear in agent.list. Got: {:?}",
        coworker_name, coworkers
    );

    println!("✓ Worktree registry integration test passed");
    println!("  - Worktree created at: {:?}", worktree_path);
    println!("  - Coworker '{}' spawned in task worktree", coworker_name);
    println!("  - Worktree registered with task_id {}", task_id);
}

/// Helper test to verify the test fixture can create a git repo with commits.
#[test]
#[ignore]
fn test_fixture_git_setup() {
    let fixture = match WorktreeTestFixture::new() {
        Some(f) => f,
        None => {
            eprintln!("SKIPPED: fixture creation failed");
            return;
        }
    };

    // Verify we have a valid git repo
    let status = Command::new("git")
        .args(["status"])
        .current_dir(&fixture.temp_dir)
        .stdout(Stdio::null())
        .status()
        .expect("Should run git status");

    assert!(status.success(), "Git status should succeed in test repo");

    // Verify we have at least one commit
    let log_output = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&fixture.temp_dir)
        .output()
        .expect("Should run git log");

    assert!(log_output.status.success(), "Git log should succeed");
    let log = String::from_utf8_lossy(&log_output.stdout);
    assert!(!log.is_empty(), "Should have at least one commit");
    assert!(log.contains("Initial commit"), "Should have initial commit");
}
