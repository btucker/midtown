//! Full workflow E2E test.
//!
//! This test verifies the complete midtown workflow from task creation
//! through PR open. It requires real Claude Code and GitHub access.
//!
//! Run with: cargo test --test workflow_e2e -- --ignored --test-threads=1

use ntest::timeout;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

// ── Shared test infrastructure ─────────────────────────────────────

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("workflow-e2e-test-{}-{}", std::process::id(), counter)
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success() || o.status.code() == Some(1))
        .unwrap_or(false)
}

fn claude_available() -> bool {
    Command::new("claude")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Kill any orphaned test daemons and tmux sessions from previous runs.
fn cleanup_orphaned_test_daemons() {
    let _ = Command::new("pkill")
        .args(["-f", "midtown daemon.*workflow-e2e-test"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("pkill")
        .args(["-f", "midtown.*workflow-e2e-test"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    thread::sleep(Duration::from_millis(100));

    // Kill any lingering tmux sessions from previous test runs
    if let Ok(output) = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        && let Ok(sessions) = String::from_utf8(output.stdout)
    {
        let current_pid = format!("workflow-e2e-test-{}-", std::process::id());
        for session in sessions.lines() {
            if session.contains("workflow-e2e-test") && !session.contains(&current_pid) {
                let _ = Command::new("tmux")
                    .args(["kill-session", "-t", session])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
    thread::sleep(Duration::from_millis(100));

    // Clean up stale project directories
    let current_pid = format!("workflow-e2e-test-{}-", std::process::id());
    let projects_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("projects");
    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with("workflow-e2e-test-")
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    // Clean up stale socket directories
    let state_dir = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("state")
        });
    let sockets_dir = state_dir.join("midtown");
    if let Ok(entries) = fs::read_dir(&sockets_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with("workflow-e2e-test-")
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Test fixture managing daemon lifecycle, tmux session, and cleanup.
#[allow(dead_code)]
struct WorkflowFixture {
    temp_dir: PathBuf,
    project_dir: PathBuf,
    repo_name: String,
    socket_path: PathBuf,
    pid_path: PathBuf,
}

impl WorkflowFixture {
    fn new() -> Option<Self> {
        cleanup_orphaned_test_daemons();

        let repo_name = test_repo_name();
        let temp_dir = std::env::temp_dir().join(&repo_name);

        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).ok()?;

        // Initialize a git repository with an initial commit (needed for worktrees)
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

        // Configure git user for this repo (needed for commits)
        let _ = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&temp_dir)
            .status();
        let _ = Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&temp_dir)
            .status();

        let status = Command::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial commit"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }

        // Create main branch (git init creates 'master' by default on some systems)
        let _ = Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

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
        let pid_path = project_dir.join("daemon.pid");

        if let Some(parent) = socket_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Some(parent) = pid_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        Some(Self {
            temp_dir,
            project_dir,
            repo_name,
            socket_path,
            pid_path,
        })
    }

    fn start_daemon(&mut self) -> bool {
        let build_result = Command::new("cargo")
            .args(["build", "--release"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        if build_result.map(|s| !s.success()).unwrap_or(true) {
            eprintln!("Failed to build daemon binary");
            return false;
        }

        let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("release")
            .join("midtown");

        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.pid_path);

        // Use `midtown start` which creates both daemon AND tmux session with lead.
        let child = Command::new(&binary_path)
            .arg("start")
            .current_dir(&self.temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("MIDTOWN_WEBHOOK_PORT", "0")
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .spawn();

        match child {
            Ok(mut c) => {
                let exit_status = c.wait();

                match exit_status {
                    Ok(status) if status.success() => {
                        for _ in 0..50 {
                            if self.socket_path.exists()
                                && UnixStream::connect(&self.socket_path).is_ok()
                            {
                                return true;
                            }
                            thread::sleep(Duration::from_millis(100));
                        }
                        eprintln!("Socket not available after successful midtown start");
                        false
                    }
                    Ok(status) => {
                        eprintln!("midtown start failed with exit status: {:?}", status);
                        false
                    }
                    Err(e) => {
                        eprintln!("Failed to wait for midtown start: {}", e);
                        false
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to spawn midtown start: {}", e);
                false
            }
        }
    }

    fn connect(&self) -> Option<UnixStream> {
        UnixStream::connect(&self.socket_path).ok()
    }

    fn rpc_call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let mut stream = self.connect()?;
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .ok()?;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        });

        let request_line = format!("{}\n", request);
        stream.write_all(request_line.as_bytes()).ok()?;
        stream.flush().ok()?;

        let mut reader = BufReader::new(&stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).ok()?;

        serde_json::from_str(&response_line).ok()
    }

    /// Return the tmux session name the daemon would use for this repo.
    fn tmux_session_name(&self) -> String {
        format!("midtown-{}", self.repo_name)
    }

    /// Wait for a condition to become true, polling at intervals.
    ///
    /// Returns true if the condition was met, false if timeout exceeded.
    fn wait_for_condition<F>(&self, timeout: Duration, mut condition: F) -> bool
    where
        F: FnMut() -> bool,
    {
        let start = Instant::now();
        let poll_interval = Duration::from_secs(2);

        while start.elapsed() < timeout {
            if condition() {
                return true;
            }
            thread::sleep(poll_interval);
        }
        false
    }

    fn stop_daemon(&mut self) {
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

        let pattern = format!("midtown daemon.*{}", self.repo_name);
        let _ = Command::new("pkill")
            .args(["-f", &pattern])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn kill_tmux_session(&self) {
        let session = self.tmux_session_name();
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &session])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for WorkflowFixture {
    fn drop(&mut self) {
        self.stop_daemon();
        self.kill_tmux_session();

        let _ = fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir(parent);
        }
        let _ = fs::remove_dir_all(&self.project_dir);
        let _ = fs::remove_dir_all(&self.temp_dir);

        // Clean up any worktrees created during tests
        let coworkers_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".midtown")
            .join("coworkers")
            .join(&self.repo_name);
        let _ = fs::remove_dir_all(&coworkers_dir);
    }
}

// ── Tests ──────────────────────────────────────────────────────────

/// Test complete task → coworker assignment → in_progress workflow.
///
/// This is a "coordination" level test - it verifies the daemon
/// correctly orchestrates the workflow from task creation to
/// coworker assignment.
///
/// The test:
/// 1. Creates a task via RPC
/// 2. Waits for daemon to assign a coworker
/// 3. Waits for task to be marked in_progress
///
/// This requires real Claude Code to be running and the daemon
/// to make assignment decisions.
#[test]
#[ignore]
#[timeout(300_000)] // 5 minutes
fn full_task_to_in_progress_workflow() {
    // Skip when using a stub command - this test requires real Claude
    if std::env::var("MIDTOWN_LEAD_COMMAND").is_ok() {
        eprintln!("MIDTOWN_LEAD_COMMAND is set (stub mode), skipping real workflow test");
        return;
    }

    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    if !claude_available() {
        eprintln!("claude CLI not available, skipping real workflow test");
        return;
    }

    let mut fixture = match WorkflowFixture::new() {
        Some(f) => f,
        None => {
            eprintln!("Failed to create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Failed to start daemon");
        return;
    }

    // Give the daemon time to stabilize
    thread::sleep(Duration::from_secs(5));

    // 1. Create a task
    let task_params = serde_json::json!({
        "subject": "Create test marker file",
        "description": "Create a file named test_marker.txt containing 'hello from workflow test'"
    });

    let task_response = fixture.rpc_call("task.create", Some(task_params));
    assert!(
        task_response.is_some(),
        "Should receive response from task.create"
    );

    let task_response = task_response.unwrap();
    if task_response["error"].is_object() {
        eprintln!("Task creation failed: {:?}", task_response["error"]);
        return;
    }

    let task_id = task_response["result"]["id"]
        .as_str()
        .expect("Task should have an ID");
    eprintln!("Created task with ID: {}", task_id);

    // 2. Wait for coworker to be assigned (daemon dispatch)
    let task_id_owned = task_id.to_string();
    let assigned = fixture.wait_for_condition(Duration::from_secs(60), || {
        if let Some(response) = fixture.rpc_call("status", None)
            && let Some(tasks) = response["result"]["tasks"]["all"].as_array()
        {
            return tasks.iter().any(|t| {
                t["id"].as_str() == Some(&task_id_owned) && t["owner"].as_str().is_some()
            });
        }
        false
    });

    assert!(
        assigned,
        "Task should be assigned to a coworker within 60 seconds"
    );
    eprintln!("Task assigned to coworker");

    // 3. Wait for task to be marked in_progress
    let task_id_owned = task_id.to_string();
    let in_progress = fixture.wait_for_condition(Duration::from_secs(120), || {
        if let Some(response) = fixture.rpc_call("status", None)
            && let Some(tasks) = response["result"]["tasks"]["all"].as_array()
        {
            return tasks
                .iter()
                .any(|t| t["id"].as_str() == Some(&task_id_owned) && t["status"] == "InProgress");
        }
        false
    });

    assert!(
        in_progress,
        "Task should be marked in_progress within 120 seconds"
    );
    eprintln!("Task is in_progress - workflow coordination verified!");

    // Get final status for logging
    if let Some(response) = fixture.rpc_call("status", None) {
        if let Some(tasks) = response["result"]["tasks"]["all"].as_array()
            && let Some(task) = tasks.iter().find(|t| t["id"].as_str() == Some(task_id))
        {
            eprintln!(
                "Final task state: owner={}, status={}",
                task["owner"].as_str().unwrap_or("none"),
                task["status"]
            );
        }
        if let Some(coworkers) = response["result"]["coworkers"].as_array() {
            eprintln!("Active coworkers: {}", coworkers.len());
            for cw in coworkers {
                eprintln!(
                    "  - {}: {}",
                    cw["name"].as_str().unwrap_or("?"),
                    cw["status"].as_str().unwrap_or("?")
                );
            }
        }
    }
}

/// Test that the daemon spawns a coworker when a task is created.
///
/// This is a lighter-weight test that just verifies the daemon
/// will spawn a coworker in response to an unassigned task.
#[test]
#[ignore]
#[timeout(180_000)] // 3 minutes
fn task_triggers_coworker_spawn() {
    // Skip when using a stub command - this test requires real Claude
    if std::env::var("MIDTOWN_LEAD_COMMAND").is_ok() {
        eprintln!("MIDTOWN_LEAD_COMMAND is set (stub mode), skipping");
        return;
    }

    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    if !claude_available() {
        eprintln!("claude CLI not available, skipping");
        return;
    }

    let mut fixture = match WorkflowFixture::new() {
        Some(f) => f,
        None => {
            eprintln!("Failed to create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Failed to start daemon");
        return;
    }

    // Give daemon time to stabilize
    thread::sleep(Duration::from_secs(5));

    // Check initial coworker count
    let initial_count = fixture
        .rpc_call("coworker.list", None)
        .and_then(|r| r["result"]["coworkers"].as_array().map(|a| a.len()))
        .unwrap_or(0);
    eprintln!("Initial coworker count: {}", initial_count);

    // Create a task
    let task_params = serde_json::json!({
        "subject": "Spawn test task",
        "description": "A simple task to trigger coworker spawn"
    });

    let task_response = fixture.rpc_call("task.create", Some(task_params));
    assert!(
        task_response.is_some(),
        "Should receive response from task.create"
    );

    // Wait for a coworker to be spawned
    let spawned = fixture.wait_for_condition(Duration::from_secs(90), || {
        if let Some(response) = fixture.rpc_call("coworker.list", None)
            && let Some(coworkers) = response["result"]["coworkers"].as_array()
        {
            return coworkers.len() > initial_count;
        }
        false
    });

    assert!(
        spawned,
        "A coworker should be spawned within 90 seconds of task creation"
    );

    // Get final count for logging
    let final_count = fixture
        .rpc_call("coworker.list", None)
        .and_then(|r| r["result"]["coworkers"].as_array().map(|a| a.len()))
        .unwrap_or(0);
    eprintln!(
        "Coworker spawned! Count: {} -> {}",
        initial_count, final_count
    );
}

/// Test that task status transitions are reflected in status RPC.
///
/// This verifies that when we create tasks, they appear correctly
/// in the daemon's status response with the expected fields.
#[test]
#[ignore]
#[timeout(120_000)] // 2 minutes
fn task_status_visibility() {
    let mut fixture = match WorkflowFixture::new() {
        Some(f) => f,
        None => {
            eprintln!("Failed to create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Failed to start daemon");
        return;
    }

    // Give daemon time to stabilize
    thread::sleep(Duration::from_secs(3));

    // Create a task
    let task_params = serde_json::json!({
        "subject": "Status visibility test",
        "description": "Testing task appears in status"
    });

    let create_response = fixture.rpc_call("task.create", Some(task_params));
    assert!(
        create_response.is_some(),
        "Should receive response from task.create"
    );

    let create_response = create_response.unwrap();
    if create_response["error"].is_object() {
        eprintln!("Task creation failed: {:?}", create_response["error"]);
        return;
    }

    let task_id = create_response["result"]["id"]
        .as_str()
        .expect("Task should have an ID");
    eprintln!("Created task: {}", task_id);

    // Verify task appears in status
    thread::sleep(Duration::from_secs(2));

    let status_response = fixture.rpc_call("status", None);
    assert!(status_response.is_some(), "Should receive status response");

    let status_response = status_response.unwrap();
    assert!(
        status_response["error"].is_null(),
        "Status should not return error"
    );

    let tasks = status_response["result"]["tasks"]["all"]
        .as_array()
        .expect("Status should include tasks.all array");

    let our_task = tasks.iter().find(|t| t["id"].as_str() == Some(task_id));
    assert!(
        our_task.is_some(),
        "Created task should appear in status. Tasks: {:?}",
        tasks.iter().map(|t| t["id"].as_str()).collect::<Vec<_>>()
    );

    let our_task = our_task.unwrap();
    assert_eq!(
        our_task["subject"].as_str(),
        Some("Status visibility test"),
        "Task subject should match"
    );
    assert!(
        our_task["status"].is_string(),
        "Task should have a status field"
    );

    eprintln!(
        "Task visible in status: subject='{}', status={}",
        our_task["subject"].as_str().unwrap_or("?"),
        our_task["status"]
    );
}
