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
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Counter for unique test names across tests.
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique test repo name to avoid conflicts.
fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("worktree-e2e-test-{}-{}", std::process::id(), counter)
}

/// Check if tmux is available.
fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Test fixture for worktree registry E2E tests.
///
/// Creates an isolated environment with a fake git repo and manages
/// daemon lifecycle. Based on the pattern from effect_verification_e2e.rs.
struct WorktreeTestFixture {
    /// Temporary directory containing the test repo
    temp_dir: PathBuf,
    /// Project directory under ~/.midtown/projects/<name>/
    project_dir: PathBuf,
    /// Worktree directory under ~/.midtown/worktrees/<name>/
    worktree_dir: PathBuf,
    /// Repository name (used for socket path derivation and tmux session)
    repo_name: String,
    /// Path to the daemon socket
    socket_path: PathBuf,
    /// Daemon process handle (if started)
    daemon_process: Option<std::process::Child>,
    /// Tmux session name (midtown-<repo_name>)
    session_name: String,
}

impl WorktreeTestFixture {
    /// Create a new test fixture with a fake git repository.
    fn new() -> Option<Self> {
        let repo_name = test_repo_name();
        let temp_dir = std::env::temp_dir().join(&repo_name);

        // Clean up any previous test data
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).ok()?;

        // Initialize a git repository (daemon requires this)
        let status = Command::new("git")
            .args(["init"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }

        // Configure git user for this repo
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;

        // Create an initial commit (required for worktree creation)
        fs::write(temp_dir.join("README.md"), "# Test repo\n").ok()?;
        Command::new("git")
            .args(["add", "."])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;

        // Compute socket path (matches midtown::paths)
        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".local")
                    .join("state")
            });
        let socket_path = state_dir
            .join("midtown")
            .join(&repo_name)
            .join("daemon.sock");

        let project_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".midtown")
            .join("projects")
            .join(&repo_name);

        let worktree_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".midtown")
            .join("worktrees")
            .join(&repo_name);

        // Ensure parent directories exist
        if let Some(parent) = socket_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Some(parent) = project_dir.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let session_name = format!("midtown-{}", repo_name);

        Some(Self {
            temp_dir,
            project_dir,
            worktree_dir,
            repo_name,
            socket_path,
            daemon_process: None,
            session_name,
        })
    }

    /// Start the daemon process.
    ///
    /// Returns true if the daemon started successfully and the socket is available.
    fn start_daemon(&mut self) -> bool {
        // Check for pre-built binary (CI builds before running tests)
        let release_binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("release")
            .join("midtown");
        let debug_binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("midtown");

        let binary_path = if release_binary.exists() {
            release_binary
        } else if debug_binary.exists() {
            eprintln!("Warning: Using debug binary - timing may not match production");
            debug_binary
        } else {
            eprintln!("Skipping: No midtown binary found. Run 'cargo build --release' first.");
            return false;
        };

        // Remove stale socket if present
        let _ = fs::remove_file(&self.socket_path);

        // Start the daemon process
        let child = Command::new(&binary_path)
            .arg("daemon")
            .arg("--workdir")
            .arg(&self.temp_dir)
            .current_dir(&self.temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Disable webhook to avoid port conflicts in tests
            .env("MIDTOWN_WEBHOOK_PORT", "0")
            // Disable chat monitor for cleaner tests
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .spawn();

        match child {
            Ok(c) => {
                self.daemon_process = Some(c);

                // Wait for socket to become available (up to 60 seconds)
                for _ in 0..300 {
                    thread::sleep(Duration::from_millis(200));
                    if self.socket_path.exists() && UnixStream::connect(&self.socket_path).is_ok() {
                        return true;
                    }
                }
                eprintln!("Daemon socket did not become available");
                false
            }
            Err(e) => {
                eprintln!("Failed to spawn daemon: {}", e);
                false
            }
        }
    }

    /// Connect to the daemon socket.
    fn connect(&self) -> Option<UnixStream> {
        UnixStream::connect(&self.socket_path).ok()
    }

    /// Send an RPC request and receive the response.
    fn rpc_call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let mut stream = self.connect()?;

        // Set read timeout to detect hangs
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .ok()?;

        // Build JSON-RPC request
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        });

        // Send request
        let request_line = format!("{}\n", request);
        stream.write_all(request_line.as_bytes()).ok()?;
        stream.flush().ok()?;

        // Read response
        let mut reader = BufReader::new(&stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).ok()?;

        // Parse response
        serde_json::from_str(&response_line).ok()
    }

    /// Get the path to a task worktree directory.
    fn task_worktree_path(&self, task_id: u32, slug: &str) -> PathBuf {
        self.worktree_dir.join(format!("task-{}-{}", task_id, slug))
    }

    /// List tmux windows in the session.
    fn list_tmux_windows(&self) -> Vec<String> {
        let output = Command::new("tmux")
            .args([
                "list-windows",
                "-t",
                &self.session_name,
                "-F",
                "#{window_name}",
            ])
            .output()
            .ok();

        match output {
            Some(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(String::from)
                .collect(),
            _ => vec![],
        }
    }

    /// Get the current working directory of a tmux pane.
    fn get_pane_cwd(&self, window_name: &str) -> Option<PathBuf> {
        let target = format!("{}:{}", self.session_name, window_name);
        let output = Command::new("tmux")
            .args([
                "display-message",
                "-t",
                &target,
                "-p",
                "#{pane_current_path}",
            ])
            .output()
            .ok()?;

        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Some(PathBuf::from(path))
        } else {
            None
        }
    }

    /// Stop the daemon gracefully.
    fn stop_daemon(&mut self) {
        // First try RPC shutdown
        if let Some(mut stream) = self.connect() {
            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "shutdown",
                "id": 999
            });
            let request_line = format!("{}\n", request);
            let _ = stream.write_all(request_line.as_bytes());
            let _ = stream.flush();
            thread::sleep(Duration::from_millis(100));
        }

        // Kill the process if still running
        if let Some(ref mut child) = self.daemon_process {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.daemon_process = None;

        // Kill the tmux session
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.session_name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        // Final fallback: pkill
        let pattern = format!("midtown daemon.*{}", self.repo_name);
        let _ = Command::new("pkill")
            .args(["-f", &pattern])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for WorktreeTestFixture {
    fn drop(&mut self) {
        self.stop_daemon();

        // Clean up socket file and its parent directory
        let _ = fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir(parent);
        }

        // Clean up the entire project directory
        let _ = fs::remove_dir_all(&self.project_dir);

        // Clean up worktree directory
        let _ = fs::remove_dir_all(&self.worktree_dir);

        // Clean up temp directory (the fake git repo)
        let _ = fs::remove_dir_all(&self.temp_dir);
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
#[ignore] // Requires tmux, built binary, and claude CLI
fn test_worktree_registry_integration_end_to_end() {
    if !tmux_available() {
        eprintln!("SKIPPED: tmux not available");
        return;
    }

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

    // The daemon should spawn a coworker for the pending task
    // We don't have control over which coworker name it picks, so we wait
    // for any coworker to appear
    let mut spawned_coworker = None;
    for _ in 0..30 {
        thread::sleep(Duration::from_secs(1));

        let windows = fixture.list_tmux_windows();
        if !windows.is_empty() {
            // Find a window that's not "lead"
            spawned_coworker = windows.into_iter().find(|w| !w.contains("lead"));
            if spawned_coworker.is_some() {
                break;
            }
        }
    }

    let coworker_name = match spawned_coworker {
        Some(name) => {
            // Extract the base coworker name (remove any status suffix)
            let base_name = name.split(':').next().unwrap_or(&name);
            println!("Coworker spawned: {}", base_name);
            base_name.to_string()
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

    // ASSERTION 2: Verify the coworker is running in the task worktree
    if let Some(cwd) = fixture.get_pane_cwd(&coworker_name) {
        assert_eq!(
            cwd, worktree_path,
            "Coworker should be running in task worktree. Expected: {:?}, Got: {:?}",
            worktree_path, cwd
        );
    } else {
        eprintln!("Warning: Could not determine coworker's working directory");
    }

    // ASSERTION 3: Verify RegisterWorktreeAssignment and BindCoworkerToWorktree effects
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

    // ASSERTION 4: Verify coworker appears in coworker.list
    let list_response = fixture.rpc_call("coworker.list", None);
    assert!(
        list_response.is_some(),
        "Should receive response from coworker.list"
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
        "Coworker '{}' should appear in coworker.list. Got: {:?}",
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
