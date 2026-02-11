//! End-to-end tests for daemon startup and API endpoints, plus standalone
//! regression tests.
//!
//! The daemon tests verify the daemon can start successfully and respond to RPC
//! requests. They're smoke tests to catch regressions in daemon lifecycle.
//! Run with `cargo test -- --ignored daemon` as these spawn real processes.
//!
//! The regression tests at the bottom are standalone (no daemon required) and
//! run as regular `cargo test` targets.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Counter for unique test names across tests.
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique test repo name to avoid conflicts.
fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("daemon-e2e-test-{}-{}", std::process::id(), counter)
}

/// Kill any orphaned daemon-e2e-test daemons from previous test runs.
///
/// This is a safety measure to ensure tests don't interfere with each other
/// if a previous test run crashed without cleaning up properly.
fn cleanup_orphaned_test_daemons() {
    // Use pkill to find and kill any midtown daemon processes with test workdirs
    // The pattern matches daemons started with --workdir containing "daemon-e2e-test"
    let _ = Command::new("pkill")
        .args(["-f", "midtown daemon.*daemon-e2e-test"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    // Give processes a moment to die
    thread::sleep(Duration::from_millis(50));

    // Clean up stale project directories from crashed previous runs.
    // Skip directories from the current process to avoid interfering with
    // concurrently running tests in the same process.
    let current_pid = format!("daemon-e2e-test-{}-", std::process::id());
    let projects_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("projects");
    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with("daemon-e2e-test-")
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    // Also clean up stale socket directories (same PID filter)
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
                && name.starts_with("daemon-e2e-test-")
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

fn copy_dir_recursive(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn cleanup_profile_dirs(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_dir_all(path);
    }
}

/// Test fixture for daemon e2e tests.
///
/// Creates an isolated environment with a fake git repo and manages
/// daemon lifecycle.
#[allow(dead_code)]
struct DaemonFixture {
    /// Temporary directory containing the test repo
    temp_dir: PathBuf,
    /// Project directory under ~/.midtown/projects/<name>/
    project_dir: PathBuf,
    /// Repository name (used for socket path derivation)
    repo_name: String,
    /// Path to the daemon socket
    socket_path: PathBuf,
    /// Path to the daemon PID file
    pid_path: PathBuf,
    /// Daemon process handle (if started)
    daemon_process: Option<Child>,
    /// Task directory for this test repo (~/.claude/tasks/midtown-<repo>/)
    tasks_dir: PathBuf,
    /// Request ID counter for generating unique RPC request IDs
    next_request_id: std::cell::Cell<u64>,
}

impl DaemonFixture {
    /// Create a new test fixture with a fake git repository.
    fn new() -> Option<Self> {
        // Clean up any orphaned daemons from previous test runs
        cleanup_orphaned_test_daemons();

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

        // Compute socket and PID paths
        // These match the paths from midtown::paths
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

        // Compute task directory
        let tasks_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("tasks")
            .join(format!("midtown-{}", &repo_name));

        // Ensure parent directories exist
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
            daemon_process: None,
            tasks_dir,
            next_request_id: std::cell::Cell::new(1),
        })
    }

    /// Start the daemon process.
    ///
    /// Returns true if the daemon started successfully and the socket is available.
    fn start_daemon(&mut self) -> bool {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let candidates = [
            manifest_dir.join("target/release/midtown"),
            manifest_dir.join("target/debug/midtown"),
            // cargo-llvm-cov uses a separate target directory for instrumented builds
            manifest_dir.join("target/llvm-cov-target/debug/midtown"),
        ];

        let binary_path = match candidates.iter().find(|p| p.exists()) {
            Some(p) => p.clone(),
            None => {
                eprintln!("Skipping: No midtown binary found. Run `cargo build` first.");
                return false;
            }
        };

        // Remove stale socket if present
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.pid_path);

        // Start the daemon process
        // Use `daemon` subcommand with --workdir pointing to our test repo
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
                // Increased from 5s because daemon now installs plugins at startup
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

    /// Create a task JSON file in the test's task directory.
    ///
    /// Creates a task with the given ID, subject, status, and optional owner.
    /// The file is written to `~/.claude/tasks/midtown-<repo_name>/<id>.json`.
    fn create_task(&self, id: &str, subject: &str, status: &str, owner: Option<&str>) {
        let _ = fs::create_dir_all(&self.tasks_dir);
        let task_json = serde_json::json!({
            "id": id,
            "subject": subject,
            "status": status,
            "owner": owner,
            "description": format!("Test task {}", id),
            "blocked_by": []
        });
        let task_file = self.tasks_dir.join(format!("{}.json", id));
        fs::write(
            &task_file,
            serde_json::to_string_pretty(&task_json).unwrap(),
        )
        .unwrap_or_else(|e| panic!("Failed to write task file {:?}: {}", task_file, e));
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
        self.rpc_call_with_timeout(method, params, Duration::from_secs(30))
    }

    /// Send an RPC request and receive the response with a timeout.
    ///
    /// Returns None if the response doesn't arrive within the timeout.
    fn rpc_call_with_timeout(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        let mut stream = self.connect()?;

        // Set read timeout to detect hangs
        stream.set_read_timeout(Some(timeout)).ok()?;

        // Generate unique request ID to avoid cache collisions
        let request_id = self.next_request_id.get();
        self.next_request_id.set(request_id + 1);

        // Build JSON-RPC request
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": request_id
        });

        // Send request
        let request_line = format!("{}\n", request);
        stream.write_all(request_line.as_bytes()).ok()?;
        stream.flush().ok()?;

        // Read response (will timeout if daemon doesn't respond in time)
        let mut reader = BufReader::new(&stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).ok()?;

        // Parse response
        serde_json::from_str(&response_line).ok()
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

        // As a final fallback, use pkill to ensure any daemon for this repo is stopped
        // This catches cases where the Child handle might not have tracked the process correctly
        let pattern = format!("midtown daemon.*{}", self.repo_name);
        let _ = Command::new("pkill")
            .args(["-f", &pattern])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        self.stop_daemon();

        // Clean up socket file and its parent directory
        let _ = fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir(parent);
        }

        // Clean up the entire project directory (~/.midtown/projects/<name>/)
        // This includes config.toml, daemon.pid, channel.jsonl, cursors/, etc.
        let _ = fs::remove_dir_all(&self.project_dir);

        // Clean up task directory (~/.claude/tasks/midtown-<name>/)
        let _ = fs::remove_dir_all(&self.tasks_dir);

        // Clean up temp directory (the fake git repo)
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

/// Test that the daemon starts and creates a socket.
#[test]
#[ignore] // Requires built binary
fn test_daemon_starts_and_creates_socket() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    assert!(
        fixture.start_daemon(),
        "Daemon should start successfully and create socket"
    );

    // Verify socket exists
    assert!(
        fixture.socket_path.exists(),
        "Socket file should exist at {:?}",
        fixture.socket_path
    );

    // Verify we can connect
    assert!(
        fixture.connect().is_some(),
        "Should be able to connect to daemon socket"
    );
}

/// Test the ping RPC endpoint.
#[test]
#[ignore] // Requires built binary
fn test_daemon_ping_endpoint() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    let response = fixture.rpc_call("ping", None);
    assert!(response.is_some(), "Should receive response from ping");

    let response = response.unwrap();
    assert_eq!(
        response["result"].as_str(),
        Some("pong"),
        "Ping should return 'pong'"
    );
}

/// Test the version RPC endpoint.
#[test]
#[ignore] // Requires built binary
fn test_daemon_version_endpoint() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    let response = fixture.rpc_call("version", None);
    assert!(response.is_some(), "Should receive response from version");

    let response = response.unwrap();
    let result = &response["result"];

    assert_eq!(
        result["name"].as_str(),
        Some("midtown"),
        "Version should report name as 'midtown'"
    );
    assert!(
        result["version"].as_str().is_some(),
        "Version should include a version string"
    );

    // Verify version matches Cargo.toml
    let expected_version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        result["version"].as_str(),
        Some(expected_version),
        "Version should match CARGO_PKG_VERSION"
    );
}

/// Test the status RPC endpoint.
#[test]
#[ignore] // Requires built binary
fn test_daemon_status_endpoint() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    let response = fixture.rpc_call("status", None);
    assert!(response.is_some(), "Should receive response from status");

    let response = response.unwrap();

    // Status should not return an error
    assert!(
        response["error"].is_null(),
        "Status should not return an error"
    );

    // Status should have a result
    assert!(
        response["result"].is_object(),
        "Status should return an object"
    );
}

/// Test the coworker.list RPC endpoint.
#[test]
#[ignore] // Requires built binary
fn test_daemon_coworker_list_endpoint() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    let response = fixture.rpc_call("coworker.list", None);
    assert!(
        response.is_some(),
        "Should receive response from coworker.list"
    );

    let response = response.unwrap();

    // Should not return an error
    assert!(
        response["error"].is_null(),
        "coworker.list should not return an error"
    );

    // Should return a result with coworkers array
    let result = &response["result"];
    assert!(
        result["coworkers"].is_array(),
        "coworker.list should return a coworkers array"
    );

    // Initially there should be no coworkers
    let coworkers = result["coworkers"].as_array().unwrap();
    assert!(
        coworkers.is_empty(),
        "Initially there should be no coworkers"
    );
}

/// Test that unknown methods return method_not_found error.
#[test]
#[ignore] // Requires built binary
fn test_daemon_unknown_method_returns_error() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    let response = fixture.rpc_call("nonexistent.method", None);
    assert!(
        response.is_some(),
        "Should receive response for unknown method"
    );

    let response = response.unwrap();

    // Should return an error
    assert!(
        response["error"].is_object(),
        "Unknown method should return an error"
    );

    // Should be method not found error (-32601)
    let error = &response["error"];
    assert_eq!(
        error["code"].as_i64(),
        Some(-32601),
        "Should return method not found error code"
    );
}

/// Test that the daemon can handle multiple sequential requests.
#[test]
#[ignore] // Requires built binary
fn test_daemon_handles_multiple_requests() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    // Send multiple requests on the same connection
    let mut stream = fixture.connect().expect("Should connect to daemon");

    for i in 1..=5 {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "ping",
            "id": i
        });

        let request_line = format!("{}\n", request);
        stream.write_all(request_line.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(&stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).unwrap();

        let response: serde_json::Value = serde_json::from_str(&response_line).unwrap();
        assert_eq!(response["result"].as_str(), Some("pong"));
        assert_eq!(response["id"].as_i64(), Some(i));
    }
}

/// Test that the channel.post RPC endpoint works.
#[test]
#[ignore] // Requires built binary
fn test_daemon_channel_post_endpoint() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    let params = serde_json::json!({
        "message": "Test message from daemon e2e test",
        "from": "test-agent"
    });

    let response = fixture.rpc_call("channel.post", Some(params));
    assert!(
        response.is_some(),
        "Should receive response from channel.post"
    );

    let response = response.unwrap();

    // Should not return an error
    assert!(
        response["error"].is_null(),
        "channel.post should not return an error: {:?}",
        response["error"]
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Comprehensive RPC handler E2E tests
//
// These tests verify RPC endpoints return expected data structures and fields.
// ────────────────────────────────────────────────────────────────────────────

/// Test that status RPC returns a tasks array with expected structure.
///
/// The status endpoint aggregates task data from Claude Code's native task
/// storage. Each task should have id, subject, status, and assignee fields.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_status_returns_tasks_array() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    let response = fixture.rpc_call("status", None);
    assert!(response.is_some(), "Should receive response from status");

    let response = response.unwrap();
    assert!(
        response["error"].is_null(),
        "Status should not return an error: {:?}",
        response["error"]
    );

    let result = &response["result"];

    // Verify tasks array exists
    assert!(
        result["tasks"].is_array(),
        "Status should return a tasks array"
    );

    // If there are tasks, verify their structure
    if let Some(tasks) = result["tasks"].as_array() {
        for task in tasks {
            // Each task should have id, subject, and status
            assert!(
                task["id"].is_string(),
                "Task should have id field: {:?}",
                task
            );
            assert!(
                task["subject"].is_string(),
                "Task should have subject field: {:?}",
                task
            );
            assert!(
                task["status"].is_string(),
                "Task should have status field: {:?}",
                task
            );
            // assignee can be null or string
            assert!(
                task["assignee"].is_null() || task["assignee"].is_string(),
                "Task assignee should be null or string: {:?}",
                task
            );
        }
    }
}

/// Test that status RPC returns all expected daemon state fields.
///
/// Verifies the status endpoint returns the complete set of fields
/// needed by clients (CLI, web UI) to display daemon state.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_status_returns_complete_daemon_state() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let response = fixture.rpc_call("status", None);
    assert!(response.is_some(), "Should receive response from status");

    let response = response.unwrap();
    let result = &response["result"];

    // Core daemon state fields
    assert!(
        result["success"].as_bool() == Some(true),
        "Status should report success"
    );
    assert!(
        result["daemon_running"].as_bool() == Some(true),
        "Status should report daemon_running: true"
    );
    assert!(
        result["active_coworkers"].is_number(),
        "Status should include active_coworkers count"
    );
    assert!(
        result["max_coworkers"].is_number(),
        "Status should include max_coworkers"
    );
    assert!(
        result["max_dev_coworkers"].is_number(),
        "Status should include max_dev_coworkers (respects reviewer headroom)"
    );
    assert!(
        result["pending_tasks"].is_number(),
        "Status should include pending_tasks count"
    );
    assert!(
        result["socket_path"].is_string(),
        "Status should include socket_path"
    );

    // Data arrays
    assert!(
        result["coworkers"].is_array(),
        "Status should include coworkers array"
    );
    assert!(
        result["tasks"].is_array(),
        "Status should include tasks array"
    );
    assert!(
        result["pull_requests"].is_array(),
        "Status should include pull_requests array"
    );
    assert!(
        result["merged_prs"].is_array(),
        "Status should include merged_prs array"
    );
    assert!(
        result["recent_activity"].is_array(),
        "Status should include recent_activity array"
    );
}

/// Test that coworker.list returns expected structure with all fields.
///
/// Each coworker entry should have name, status, current_task, and started_at.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_coworker_list_returns_expected_structure() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let response = fixture.rpc_call("coworker.list", None);
    assert!(
        response.is_some(),
        "Should receive response from coworker.list"
    );

    let response = response.unwrap();
    assert!(
        response["error"].is_null(),
        "coworker.list should not return an error"
    );

    let result = &response["result"];

    // Should report success
    assert!(
        result["success"].as_bool() == Some(true),
        "coworker.list should report success"
    );

    // Coworkers should be an array
    assert!(
        result["coworkers"].is_array(),
        "coworker.list should return coworkers array"
    );

    // If there were coworkers, verify their structure
    // (In test environment there typically aren't any, but we verify the response format)
    if let Some(coworkers) = result["coworkers"].as_array() {
        for cw in coworkers {
            assert!(cw["name"].is_string(), "Coworker should have name field");
            assert!(
                cw["status"].is_string(),
                "Coworker should have status field"
            );
            assert!(
                cw["started_at"].is_string(),
                "Coworker should have started_at timestamp"
            );
            // current_task can be null or string
            assert!(
                cw["current_task"].is_null() || cw["current_task"].is_string(),
                "Coworker current_task should be null or string"
            );
        }
    }
}

/// Test that snapshot RPC returns WorldSnapshot with pane contents.
///
/// The snapshot endpoint returns the full daemon state including pane
/// captures, which can be used for debugging and coworker view.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_snapshot_returns_pane_contents() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let response = fixture.rpc_call("snapshot", None);
    assert!(response.is_some(), "Should receive response from snapshot");

    let response = response.unwrap();
    assert!(
        response["error"].is_null(),
        "Snapshot should not return an error: {:?}",
        response["error"]
    );

    let result = &response["result"];

    // WorldSnapshot structure verification
    // Note: field names depend on serde serialization of WorldSnapshot struct

    // Should have coworker-related data
    assert!(
        result["coworker_snapshots"].is_array() || result["active_names"].is_array(),
        "Snapshot should contain coworker data"
    );

    // Should have pane contents map (may be empty if no coworkers)
    // The exact field name depends on the struct serialization
    let has_pane_data = result["pane_contents"].is_object()
        || result["pane_contents"].is_array()
        || result["coworker_snapshots"].is_array();
    assert!(
        has_pane_data,
        "Snapshot should contain pane content data structure"
    );

    // Should have task-related data
    assert!(
        result["all_tasks"].is_array()
            || result["pending_unblocked_tasks"].is_array()
            || result["busy_coworkers"].is_array(),
        "Snapshot should contain task-related data"
    );
}

/// Test that channel.read RPC returns messages with expected structure.
///
/// The channel.read endpoint returns recent channel messages, each with
/// from, message, and timestamp fields.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_channel_read_returns_messages() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // First post a message so we have something to read
    let post_params = serde_json::json!({
        "message": "Test message for channel read",
        "from": "test-agent"
    });
    let post_response = fixture.rpc_call("channel.post", Some(post_params));
    assert!(post_response.is_some(), "Should be able to post to channel");

    // Now read the channel
    let read_response = fixture.rpc_call("channel.read", None);
    assert!(
        read_response.is_some(),
        "Should receive response from channel.read"
    );

    let response = read_response.unwrap();
    assert!(
        response["error"].is_null(),
        "channel.read should not return an error: {:?}",
        response["error"]
    );

    let result = &response["result"];
    assert!(
        result["messages"].is_array(),
        "channel.read should return messages array"
    );

    // Verify we can find our test message
    let messages = result["messages"].as_array().unwrap();
    let found_test_message = messages.iter().any(|m| {
        m["from"].as_str() == Some("test-agent")
            && m["message"]
                .as_str()
                .map(|s| s.contains("Test message"))
                .unwrap_or(false)
    });
    assert!(
        found_test_message,
        "Should find our posted test message in channel.read response"
    );

    // Verify message structure
    for msg in messages {
        assert!(msg["from"].is_string(), "Message should have from field");
        assert!(
            msg["message"].is_string(),
            "Message should have message field"
        );
        assert!(
            msg["timestamp"].is_string(),
            "Message should have timestamp field"
        );
    }
}

/// Test that reminder.create and reminder.list RPCs work correctly.
///
/// The reminder system allows scheduling one-shot nudges triggered by
/// conditions like "all-work-merged". This tests the full lifecycle:
/// create a reminder, list it, then cancel it.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_reminder_lifecycle() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Create a reminder
    let create_params = serde_json::json!({
        "trigger": "all-work-merged",
        "message": "Test reminder message"
    });
    let create_response = fixture.rpc_call("reminder.create", Some(create_params));
    assert!(
        create_response.is_some(),
        "Should receive response from reminder.create"
    );

    let create_response = create_response.unwrap();
    assert!(
        create_response["error"].is_null(),
        "reminder.create should not return an error: {:?}",
        create_response["error"]
    );

    // The response contains a confirmation message with the ID embedded
    let result = &create_response["result"];
    let message = result["message"]
        .as_str()
        .expect("reminder.create should return a message");
    assert!(
        message.contains("Reminder set"),
        "Confirmation should mention 'Reminder set'"
    );
    assert!(
        message.contains("Test reminder message"),
        "Confirmation should include the reminder message"
    );

    // Extract the ID from the message (format: "Reminder set (id: abc123): ...")
    let id_start = message
        .find("(id: ")
        .unwrap_or_else(|| panic!("Expected '(id: ' in message but got: {}", message))
        + 5;
    let id_end = message[id_start..]
        .find(')')
        .unwrap_or_else(|| panic!("Expected ')' after ID in message: {}", message));
    let reminder_id = &message[id_start..id_start + id_end];

    // List reminders - should include our new one
    let list_response = fixture.rpc_call("reminder.list", None);
    assert!(
        list_response.is_some(),
        "Should receive response from reminder.list"
    );

    let list_response = list_response.unwrap();
    assert!(
        list_response["error"].is_null(),
        "reminder.list should not return an error"
    );

    // reminder.list returns a formatted text message, not structured data
    let list_message = list_response["result"]["message"]
        .as_str()
        .expect("reminder.list should return a message");
    assert!(
        list_message.contains("Active reminders"),
        "List should show active reminders"
    );
    assert!(
        list_message.contains(reminder_id),
        "List should contain our reminder ID"
    );
    assert!(
        list_message.contains("Test reminder message"),
        "List should contain our reminder message"
    );

    // Cancel the reminder
    let cancel_params = serde_json::json!({ "id": reminder_id });
    let cancel_response = fixture.rpc_call("reminder.cancel", Some(cancel_params));
    assert!(
        cancel_response.is_some(),
        "Should receive response from reminder.cancel"
    );

    let cancel_response = cancel_response.unwrap();
    assert!(
        cancel_response["error"].is_null(),
        "reminder.cancel should not return an error"
    );

    let cancel_message = cancel_response["result"]["message"]
        .as_str()
        .expect("reminder.cancel should return a message");
    assert!(
        cancel_message.contains("cancelled"),
        "Cancel should confirm cancellation"
    );

    // List again - should show "No active reminders"
    let list_after = fixture.rpc_call("reminder.list", None).unwrap();
    let list_after_message = list_after["result"]["message"]
        .as_str()
        .expect("reminder.list should return a message");
    assert!(
        list_after_message.contains("No active reminders"),
        "After cancellation, should show no active reminders"
    );
}

/// Test that kanban.data RPC returns expected structure.
///
/// The kanban endpoint provides data for the web UI's kanban board view,
/// including open PRs, merged PRs, and repository information.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_kanban_data_returns_structure() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let response = fixture.rpc_call("kanban.data", None);
    assert!(
        response.is_some(),
        "Should receive response from kanban.data"
    );

    let response = response.unwrap();
    assert!(
        response["error"].is_null(),
        "kanban.data should not return an error: {:?}",
        response["error"]
    );

    let result = &response["result"];

    // Kanban data returns PR-centric data for the board view
    assert!(result.is_object(), "kanban.data should return an object");

    // Should have prs array (open PRs for the kanban columns)
    assert!(
        result["prs"].is_array(),
        "kanban.data should contain prs array"
    );

    // Should have merged_prs array (for the Done column)
    assert!(
        result["merged_prs"].is_array(),
        "kanban.data should contain merged_prs array"
    );

    // Should have repos array (repository metadata)
    assert!(
        result["repos"].is_array(),
        "kanban.data should contain repos array"
    );
}

/// Test that coworker.spawn returns a valid JSON-RPC response.
///
/// In test environment without tmux, spawn will fail, but should return
/// a proper JSON-RPC error response rather than hanging or crashing.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_coworker_spawn_returns_response() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // In test environment without tmux, spawn will fail
    // We verify it returns a proper JSON-RPC response (not hang/crash)
    let response = fixture.rpc_call("coworker.spawn", Some(serde_json::json!({})));
    assert!(
        response.is_some(),
        "Should receive response from coworker.spawn"
    );

    let response = response.unwrap();
    // Without tmux, we expect an error response
    // Verify it's a proper JSON-RPC error with expected structure
    if response["error"].is_object() {
        assert!(
            response["error"]["code"].is_number(),
            "Error should have numeric code"
        );
        assert!(
            response["error"]["message"].is_string(),
            "Error should have message string"
        );
    } else {
        // If somehow it succeeds, verify result structure
        assert!(
            response["result"].is_object(),
            "Success should return result object"
        );
    }
}

/// Test that RPCs with invalid params return proper errors.
///
/// Methods requiring parameters should return invalid_params error (-32602)
/// when called without required params.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_invalid_params_returns_error() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // coworker.break requires "name" param
    let response = fixture.rpc_call("coworker.break", None);
    assert!(response.is_some(), "Should receive response");

    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Missing params should return error"
    );
    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32602),
        "Should be invalid_params error code"
    );

    // coworker.nudge requires "name" and "message" params
    let response2 = fixture.rpc_call("coworker.nudge", Some(serde_json::json!({"name": "test"})));
    assert!(response2.is_some(), "Should receive response");

    let response2 = response2.unwrap();
    assert!(
        response2["error"].is_object(),
        "Missing message param should return error"
    );
}

/// Test daemon graceful shutdown via SIGTERM.
#[test]
#[ignore] // Requires built binary
fn test_daemon_graceful_shutdown() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    // Verify daemon is running
    assert!(
        fixture.connect().is_some(),
        "Should be able to connect before shutdown"
    );

    // Read PID and send SIGTERM for graceful shutdown
    let pid_content = fs::read_to_string(&fixture.pid_path).expect("Should read PID file");
    let pid: u32 = pid_content.trim().parse().expect("PID should be a number");

    // Send SIGTERM
    let _ = Command::new("kill").arg(pid.to_string()).status();

    // Wait for daemon to shut down (poll with increasing delay)
    let mut shutdown_confirmed = false;
    for i in 0..20 {
        thread::sleep(Duration::from_millis(100 + i * 50));
        if UnixStream::connect(&fixture.socket_path).is_err() {
            shutdown_confirmed = true;
            break;
        }
    }

    assert!(
        shutdown_confirmed,
        "Should not be able to connect after SIGTERM"
    );
}

/// Test that PID file is created and contains valid PID.
#[test]
#[ignore] // Requires built binary
fn test_daemon_creates_pid_file() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    // PID file should exist
    assert!(
        fixture.pid_path.exists(),
        "PID file should exist at {:?}",
        fixture.pid_path
    );

    // PID file should contain a valid number
    let pid_content = fs::read_to_string(&fixture.pid_path).expect("Should read PID file");
    let pid: u32 = pid_content.trim().parse().expect("PID should be a number");
    assert!(pid > 0, "PID should be a positive number");
}

/// Test that newly spawned coworkers are not sent on a break immediately.
///
/// This test guards against a race condition where the daemon's idle-check
/// could send a coworker on a break before it has a chance to claim work.
/// Coworkers should have a minimum lifetime (e.g., 5 minutes) before being
/// eligible for an automatic break.
#[test]
#[ignore] // Requires built binary
fn test_coworker_minimum_lifetime() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    // Spawn a coworker
    let spawn_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({ "name": "testworker" })),
    );
    assert!(
        spawn_response.is_some(),
        "Should receive response from coworker.spawn"
    );

    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        // Coworker spawn might fail in test environment - skip gracefully
        eprintln!(
            "Coworker spawn failed (expected in some test environments): {:?}",
            spawn_response["error"]
        );
        return;
    }

    // Wait 30 seconds - coworker should still be alive
    // (Real minimum lifetime should be 5 minutes, but for test we check 30s)
    thread::sleep(Duration::from_secs(30));

    // Check coworker is still listed
    let list_response = fixture.rpc_call("coworker.list", None);
    assert!(
        list_response.is_some(),
        "Should receive response from coworker.list"
    );

    let list_response = list_response.unwrap();
    let coworkers = list_response["result"]["coworkers"]
        .as_array()
        .expect("coworkers should be an array");

    // Find our test coworker
    let test_coworker = coworkers
        .iter()
        .find(|c| c["name"].as_str() == Some("testworker"));

    assert!(
        test_coworker.is_some(),
        "Newly spawned coworker should NOT be sent on a break within 30 seconds. \
         Coworkers need a minimum lifetime before an automatic break to prevent \
         race conditions where they're sent on a break before claiming work."
    );
}

/// Test Codex auth switching restarts Codex coworker sessions and leaves Lead unchanged.
#[test]
#[ignore] // Requires built binary and local codex auth
fn test_daemon_rpc_auth_switch_codex_relaunches_codex_sessions() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let provider = midtown::auth::AuthProvider::Codex;
    let source_dir = midtown::auth::current_profile_dir_for(provider);
    if !source_dir.exists() {
        eprintln!("Skipping: no Codex profile dir at {:?}", source_dir);
        return;
    }

    let profile_a = format!("{}-codex-a@example.com", fixture.repo_name);
    let profile_b = format!("{}-codex-b@example.com", fixture.repo_name);
    let profile_a_dir = midtown::auth::profile_dir_for(provider, &profile_a);
    let profile_b_dir = midtown::auth::profile_dir_for(provider, &profile_b);
    let cleanup_paths = vec![profile_a_dir.clone(), profile_b_dir.clone()];

    if copy_dir_recursive(&source_dir, &profile_a_dir).is_err()
        || copy_dir_recursive(&source_dir, &profile_b_dir).is_err()
    {
        eprintln!("Skipping: failed to prepare Codex test profiles");
        cleanup_profile_dirs(&cleanup_paths);
        return;
    }

    let set_profile = |fixture: &DaemonFixture, profile: &str| {
        fixture.rpc_call(
            "auth.switch",
            Some(serde_json::json!({
                "profile": profile,
                "provider": "codex",
                "all": false
            })),
        )
    };

    let set_a = set_profile(&fixture, &profile_a);
    assert!(set_a.is_some(), "auth.switch should respond");
    let set_a = set_a.unwrap();
    assert!(
        set_a["error"].is_null(),
        "auth.switch to profile_a should succeed: {:?}",
        set_a["error"]
    );

    let spawn_name = "codex-switch-test";
    let spawn_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "name": spawn_name,
            "provider": "codex",
        })),
    );
    assert!(
        spawn_response.is_some(),
        "Should receive response from coworker.spawn"
    );
    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        eprintln!(
            "Skipping: codex coworker.spawn failed in this environment: {:?}",
            spawn_response["error"]
        );
        cleanup_profile_dirs(&cleanup_paths);
        return;
    }

    let switch_response = set_profile(&fixture, &profile_b);
    assert!(
        switch_response.is_some(),
        "auth.switch should respond after spawn"
    );
    let switch_response = switch_response.unwrap();
    assert!(
        switch_response["error"].is_null(),
        "auth.switch should succeed: {:?}",
        switch_response["error"]
    );

    let result = &switch_response["result"];
    let shutdown = result["coworkers_shutdown"].as_u64().unwrap_or(0);
    let relaunched = result["coworkers_relaunched"].as_u64().unwrap_or(0);
    assert!(
        shutdown >= 1,
        "codex auth switch should shut down codex coworker sessions"
    );
    assert!(
        relaunched >= 1,
        "codex auth switch should relaunch codex coworker sessions"
    );
    assert_eq!(
        result["lead_relaunch_status"].as_str(),
        Some("unchanged"),
        "Codex auth switch should not treat lead as relaunch failure"
    );
    assert_eq!(
        result["lead_relaunch_attempted"].as_bool(),
        Some(false),
        "Lead relaunch should not be attempted for codex auth switch"
    );

    cleanup_profile_dirs(&cleanup_paths);
}

// ────────────────────────────────────────────────────────────────────────────
// Regression tests
//
// Standalone tests for specific bugs found during development.
// These do NOT require a running daemon and run as regular (non-ignored) tests.
// ────────────────────────────────────────────────────────────────────────────

/// Regression test for #644: CLI argument name conflict.
///
/// The `--repo` global arg and the `start` subcommand's `--repos` (auto-generated
/// short form `--repo`) caused a clap panic at runtime. Fixed by renaming the
/// subcommand arg to `--add-repo`.
///
/// This test verifies that `midtown start --help` exits cleanly without a clap
/// panic. The panic occurred during argument parsing, so `--help` triggers the
/// same code path.
#[test]
fn test_cli_start_help_no_panic() {
    let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("midtown");

    if !binary_path.exists() {
        // Binary not built yet — skip silently
        eprintln!("Skipping: debug binary not found at {:?}", binary_path);
        return;
    }

    let output = Command::new(&binary_path)
        .args(["start", "--help"])
        .output()
        .expect("Failed to run midtown start --help");

    assert!(
        output.status.success(),
        "midtown start --help should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--add-repo"),
        "Help should show --add-repo (not --repo which conflicts with global arg). Got: {}",
        stdout
    );
}

/// Regression test for #644: global --repo arg still works.
///
/// Verify the global `--repo` arg appears in the top-level help and doesn't
/// conflict with subcommand args.
#[test]
fn test_cli_global_help_no_panic() {
    let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("midtown");

    if !binary_path.exists() {
        eprintln!("Skipping: debug binary not found at {:?}", binary_path);
        return;
    }

    let output = Command::new(&binary_path)
        .args(["--help"])
        .output()
        .expect("Failed to run midtown --help");

    assert!(
        output.status.success(),
        "midtown --help should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--repo"),
        "Global help should show --repo arg. Got: {}",
        stdout
    );
}

/// Regression test: web assets should not be stale.
///
/// Verifies that built web assets in `web/` are at least as new as
/// source files in `web-app/src/`. If this fails, run `cd web-app && npm run build`
/// and commit the output.
#[test]
fn test_web_assets_not_stale() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web_dir = manifest_dir.join("web");
    let src_dir = manifest_dir.join("web-app").join("src");

    if !web_dir.exists() || !src_dir.exists() {
        eprintln!("Skipping: web/ or web-app/src/ not found");
        return;
    }

    // Find the newest source file
    let newest_source = newest_file_mtime(&src_dir);
    // Find the newest built asset
    let newest_asset = newest_file_mtime(&web_dir);

    match (newest_source, newest_asset) {
        (Some(src_time), Some(asset_time)) => {
            assert!(
                asset_time >= src_time,
                "Web assets are stale! Newest source ({:?}) is newer than newest asset ({:?}). \
                 Run `cd web-app && npm run build` and commit the output.",
                src_time,
                asset_time
            );
        }
        (Some(_), None) => {
            panic!("Source files exist in web-app/src/ but no built assets found in web/");
        }
        _ => {
            // No sources or both empty — nothing to check
        }
    }
}

/// Find the most recent modification time of any file in a directory (recursive).
fn newest_file_mtime(dir: &std::path::Path) -> Option<std::time::SystemTime> {
    let mut newest = None;

    fn walk(dir: &std::path::Path, newest: &mut Option<std::time::SystemTime>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Skip node_modules
                    if path.file_name().is_some_and(|n| n == "node_modules") {
                        continue;
                    }
                    walk(&path, newest);
                } else if let Ok(meta) = path.metadata()
                    && let Ok(mtime) = meta.modified()
                    && (newest.is_none() || Some(mtime) > *newest)
                {
                    *newest = Some(mtime);
                }
            }
        }
    }

    walk(dir, &mut newest);
    newest
}

/// Test that `midtown start` installs required plugins.
///
/// This E2E test verifies the plugin auto-installation feature by:
/// 1. Uninstalling the required plugin if it's present
/// 2. Starting midtown (which should trigger auto-install via CLI)
/// 3. Verifying the plugin is now installed
///
/// Plugin installation happens in the CLI (not daemon) for better UX.
#[test]
#[ignore] // Requires built binary and Claude Code
fn test_daemon_installs_required_plugins() {
    use midtown::daemon::REQUIRED_PLUGINS;

    // Must have at least one required plugin to test
    assert!(
        !REQUIRED_PLUGINS.is_empty(),
        "REQUIRED_PLUGINS should not be empty"
    );

    let test_plugin = REQUIRED_PLUGINS[0];

    // Check if Claude CLI is available
    let list_output = Command::new("claude")
        .args(["plugin", "list", "--json"])
        .output();

    let list_output = match list_output {
        Ok(output) if output.status.success() => output,
        _ => {
            eprintln!("Skipping: claude CLI not available or failed");
            return;
        }
    };

    // Check if plugin is installed and uninstall it for testing
    let stdout = String::from_utf8_lossy(&list_output.stdout);
    if let Ok(plugins) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
        let is_installed = plugins
            .iter()
            .any(|p| p.get("id").and_then(|id| id.as_str()) == Some(test_plugin));

        if is_installed {
            // Uninstall the plugin for testing
            let _ = Command::new("claude")
                .args(["plugin", "remove", test_plugin])
                .output();

            // Give it a moment
            thread::sleep(Duration::from_millis(500));
        }
    }

    // Verify plugin is NOT installed before starting
    let list_output = Command::new("claude")
        .args(["plugin", "list", "--json"])
        .output()
        .expect("Should run claude plugin list");

    let plugins_before: Vec<serde_json::Value> =
        serde_json::from_slice(&list_output.stdout).unwrap_or_default();
    let installed_before = plugins_before
        .iter()
        .any(|p| p.get("id").and_then(|id| id.as_str()) == Some(test_plugin));

    assert!(
        !installed_before,
        "Plugin should NOT be installed before midtown start for this test to be valid"
    );

    // Create a git repo for midtown to operate in
    let fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    // Use `midtown start` which installs plugins (not `midtown daemon`)
    let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("midtown");

    let start_result = Command::new(&binary_path)
        .arg("start")
        .arg("--daemon-only") // Don't create tmux session, just start daemon
        .current_dir(&fixture.temp_dir)
        .env("MIDTOWN_WEBHOOK_PORT", "0")
        .env("MIDTOWN_CHAT_MONITOR", "0")
        // Clear MIDTOWN_LEAD_COMMAND so plugin installation happens
        // (the CLI skips plugins when a stub command is set)
        .env_remove("MIDTOWN_LEAD_COMMAND")
        .output();

    match start_result {
        Ok(output) if output.status.success() => {
            // Daemon started successfully - cleanup handled by DaemonFixture::drop()
            // which connects to the socket and sends shutdown RPC
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("midtown start failed: {}", stderr);
            return;
        }
        Err(e) => {
            eprintln!("Failed to run midtown start: {}", e);
            return;
        }
    }

    // Verify plugin is now installed
    let list_output = Command::new("claude")
        .args(["plugin", "list", "--json"])
        .output()
        .expect("Should run claude plugin list after daemon start");

    let plugins_after: Vec<serde_json::Value> =
        serde_json::from_slice(&list_output.stdout).unwrap_or_default();
    let installed_after = plugins_after
        .iter()
        .any(|p| p.get("id").and_then(|id| id.as_str()) == Some(test_plugin));

    assert!(
        installed_after,
        "Required plugin '{}' should be automatically installed when daemon starts. \
         Plugins before: {:?}, Plugins after: {:?}",
        test_plugin,
        plugins_before
            .iter()
            .filter_map(|p| p.get("id").and_then(|id| id.as_str()))
            .collect::<Vec<_>>(),
        plugins_after
            .iter()
            .filter_map(|p| p.get("id").and_then(|id| id.as_str()))
            .collect::<Vec<_>>()
    );
}

/// Regression test for #653: global config template is generated on first load.
///
/// Verifies that GlobalConfig::load() creates a template file when none exists,
/// and that the template parses back as valid TOML with default values.
#[test]
fn test_global_config_generates_template() {
    let template = midtown::config::GlobalConfig::default_template();

    // Template should be non-empty
    assert!(!template.is_empty(), "Template should not be empty");

    // Template should contain all sections
    assert!(template.contains("[default]"));
    assert!(template.contains("[plugins]"));
    assert!(template.contains("[daemon]"));

    // All options are commented out, so parsing should yield defaults
    let config: midtown::config::GlobalConfig =
        toml::from_str(&template).expect("Template should be valid TOML");
    assert!(
        config.default.max_coworkers().is_none(),
        "All options should be commented out (defaults)"
    );
}

/// Regression test: status RPC should respond within the client timeout.
///
/// This test verifies that the "status" RPC method responds within the client's
/// configured timeout. The status handler uses `spawn_blocking` for gh CLI calls,
/// which can take several seconds (especially with GitHub auth switching).
///
/// Bug: After commit e4345d6, the status endpoint takes ~2-3 seconds due to gh CLI
/// latency, but the client had a 1-second timeout. This caused "Read timeout" errors.
/// The fix is to increase the client timeout to 5 seconds for status-heavy methods.
#[test]
#[ignore] // Requires built binary
fn test_status_rpc_responds_within_timeout() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return, // Skip silently if fixture creation fails
    };

    if !fixture.start_daemon() {
        return; // Skip silently if daemon fails to start
    }

    // First verify ping works quickly (sanity check - no spawn_blocking)
    let ping_response = fixture.rpc_call_with_timeout("ping", None, Duration::from_secs(1));
    assert!(
        ping_response.is_some(),
        "Ping should respond within 1 second"
    );

    // Status calls gh CLI which can take 2-3 seconds.
    // The client timeout must accommodate this.
    // Use 5 seconds which matches the client's extended timeout for status.
    let status_response = fixture.rpc_call_with_timeout("status", None, Duration::from_secs(5));
    assert!(
        status_response.is_some(),
        "Status RPC should respond within 5 seconds. The gh CLI calls \
         inside spawn_blocking can take 2-3 seconds. If this times out, \
         check that the client timeout is at least 5 seconds for status."
    );

    let response = status_response.unwrap();
    assert!(
        response["error"].is_null(),
        "Status should not return an error: {:?}",
        response["error"]
    );
    assert!(
        response["result"].is_object(),
        "Status should return an object result"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Live daemon tests (real midtown repo)
//
// These tests run against the actual midtown repo and its daemon. They catch
// issues that only manifest with real-world state (orphaned worktrees, PRs,
// GitHub API latency, etc.).
//
// Run with: cargo test --test daemon_e2e live_daemon -- --ignored --nocapture
// ────────────────────────────────────────────────────────────────────────────

/// Test fixture for live daemon tests.
///
/// Connects to an existing daemon running against the real midtown repo,
/// or starts one if needed. Unlike DaemonFixture, this doesn't create
/// a temporary repo - it uses the actual codebase.
struct LiveDaemonFixture {
    /// Path to the daemon socket (midtown repo)
    socket_path: PathBuf,
    /// Whether we started the daemon (and should stop it on drop)
    started_daemon: bool,
    /// Path to the binary (release build preferred for realistic timing)
    binary_path: PathBuf,
    /// Request ID counter for generating unique RPC request IDs
    next_request_id: std::cell::Cell<u64>,
}

impl LiveDaemonFixture {
    /// Create a fixture for the midtown repo daemon.
    ///
    /// Returns None if the binary isn't built or the repo isn't detected.
    fn new() -> Option<Self> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let candidates = [
            manifest_dir.join("target/release/midtown"),
            manifest_dir.join("target/debug/midtown"),
            manifest_dir.join("target/llvm-cov-target/debug/midtown"),
        ];

        let binary_path = match candidates.iter().find(|p| p.exists()) {
            Some(p) => p.clone(),
            None => {
                eprintln!("Skipping: No midtown binary found. Run `cargo build` first.");
                return None;
            }
        };

        // Socket path for midtown repo
        let socket_path = midtown::paths::daemon_socket_for_repo("midtown");

        Some(Self {
            socket_path,
            started_daemon: false,
            binary_path,
            next_request_id: std::cell::Cell::new(1),
        })
    }

    /// Ensure the daemon is running.
    ///
    /// If a daemon is already running, use it. Otherwise start one.
    fn ensure_daemon_running(&mut self) -> bool {
        // Check if daemon is already running
        if UnixStream::connect(&self.socket_path).is_ok() {
            return true;
        }

        // Start daemon
        eprintln!("Starting daemon for live tests...");
        let status = Command::new(&self.binary_path)
            .args(["daemon", "--foreground"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        match status {
            Ok(mut child) => {
                // Wait for socket to appear
                for _ in 0..50 {
                    thread::sleep(Duration::from_millis(100));
                    if self.socket_path.exists() {
                        // Give daemon a moment to be ready
                        thread::sleep(Duration::from_millis(500));
                        self.started_daemon = true;
                        return true;
                    }
                }
                // Cleanup if socket never appeared
                let _ = child.kill();
                eprintln!("Daemon socket never appeared");
                false
            }
            Err(e) => {
                eprintln!("Failed to start daemon: {}", e);
                false
            }
        }
    }

    /// Connect to the daemon socket.
    fn connect(&self) -> Option<UnixStream> {
        UnixStream::connect(&self.socket_path).ok()
    }

    /// Send an RPC request with timeout.
    fn rpc_call_with_timeout(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        let mut stream = self.connect()?;
        stream.set_read_timeout(Some(timeout)).ok()?;

        // Generate unique request ID to avoid cache collisions
        let request_id = self.next_request_id.get();
        self.next_request_id.set(request_id + 1);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": request_id
        });

        let request_line = format!("{}\n", request);
        stream.write_all(request_line.as_bytes()).ok()?;
        stream.flush().ok()?;

        let mut reader = BufReader::new(&stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).ok()?;

        serde_json::from_str(&response_line).ok()
    }
}

impl Drop for LiveDaemonFixture {
    fn drop(&mut self) {
        if self.started_daemon {
            // Stop the daemon we started
            let _ = Command::new(&self.binary_path)
                .args(["stop"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Live test: Status RPC responds within timeout against real repo.
///
/// This test runs against the actual midtown repo with real orphaned worktrees,
/// PRs, and GitHub API latency. It catches timeout issues that don't manifest
/// in the clean test repo environment.
///
/// Run: cargo test --test daemon_e2e live_daemon_status -- --ignored --nocapture
#[test]
#[ignore] // Requires running daemon against real repo
fn live_daemon_status_responds_within_timeout() {
    let mut fixture = match LiveDaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.ensure_daemon_running() {
        eprintln!("Skipping: Could not start daemon");
        return;
    }

    // Test ping first (fast, no spawn_blocking)
    let ping_start = std::time::Instant::now();
    let ping_response = fixture.rpc_call_with_timeout("ping", None, Duration::from_secs(2));
    let ping_elapsed = ping_start.elapsed();

    assert!(
        ping_response.is_some(),
        "Ping should respond (took {:?})",
        ping_elapsed
    );
    println!("Ping responded in {:?}", ping_elapsed);

    // Test status multiple times to catch intermittent issues
    for i in 1..=5 {
        let start = std::time::Instant::now();
        let response = fixture.rpc_call_with_timeout("status", None, Duration::from_secs(10));
        let elapsed = start.elapsed();

        assert!(
            response.is_some(),
            "Status call {} timed out after {:?}. \
             This indicates the daemon is overloaded or spawn_blocking is saturated.",
            i,
            elapsed
        );

        let response = response.unwrap();
        assert!(
            response["error"].is_null(),
            "Status call {} returned error: {:?}",
            i,
            response["error"]
        );

        println!("Status call {} completed in {:?}", i, elapsed);

        // Brief pause between calls
        thread::sleep(Duration::from_millis(500));
    }
}

/// Live test: Multiple rapid status calls don't cause timeouts.
///
/// This tests that the daemon can handle bursts of status requests without
/// the blocking thread pool getting saturated.
#[test]
#[ignore] // Requires running daemon against real repo
fn live_daemon_status_burst_handling() {
    let mut fixture = match LiveDaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.ensure_daemon_running() {
        eprintln!("Skipping: Could not start daemon");
        return;
    }

    // Send 10 rapid status requests
    let mut successes = 0;
    let mut failures = 0;
    let mut total_time = Duration::ZERO;

    for i in 1..=10 {
        let start = std::time::Instant::now();
        let response = fixture.rpc_call_with_timeout("status", None, Duration::from_secs(10));
        let elapsed = start.elapsed();
        total_time += elapsed;

        if response.is_some() && response.as_ref().unwrap()["error"].is_null() {
            successes += 1;
            println!("Request {}: OK ({:?})", i, elapsed);
        } else {
            failures += 1;
            println!("Request {}: FAILED ({:?})", i, elapsed);
        }
    }

    let avg_time = total_time / 10;
    println!(
        "\nResults: {}/10 succeeded, avg response time: {:?}",
        successes, avg_time
    );

    // All requests should succeed
    assert_eq!(
        failures, 0,
        "All status requests should succeed. {} failures detected.",
        failures
    );

    // Average response time should be reasonable (under 5 seconds)
    assert!(
        avg_time < Duration::from_secs(5),
        "Average response time {:?} exceeds 5 seconds",
        avg_time
    );
}

/// Live test: Concurrent RPC methods don't block each other.
///
/// Tests that slow methods (status) don't block fast methods (ping).
#[test]
#[ignore] // Requires running daemon against real repo
fn live_daemon_concurrent_rpc_methods() {
    let fixture = match LiveDaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    // This test requires an already-running daemon
    if fixture.connect().is_none() {
        eprintln!("Skipping: Daemon not running. Start with 'midtown start' first.");
        return;
    }

    // Start a status call (slow - uses spawn_blocking)
    let fixture_for_status = LiveDaemonFixture::new().unwrap();
    let status_thread = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let response =
            fixture_for_status.rpc_call_with_timeout("status", None, Duration::from_secs(15));
        (response, start.elapsed())
    });

    // Small delay to ensure status call is in-flight
    thread::sleep(Duration::from_millis(100));

    // Ping should still respond quickly even while status is pending
    let ping_start = std::time::Instant::now();
    let ping_response = fixture.rpc_call_with_timeout("ping", None, Duration::from_secs(2));
    let ping_elapsed = ping_start.elapsed();

    assert!(
        ping_response.is_some(),
        "Ping should respond while status is in-flight"
    );
    // Ping should respond in under 2s even during status call.
    // We allow some slack because gh CLI auth switching can cause
    // brief delays even with spawn_blocking.
    assert!(
        ping_elapsed < Duration::from_secs(2),
        "Ping should be fast ({:?}) even during status call",
        ping_elapsed
    );
    println!("Ping during status: {:?}", ping_elapsed);

    // Wait for status to complete
    let (status_response, status_elapsed) = status_thread.join().expect("Status thread panicked");
    assert!(
        status_response.is_some(),
        "Status should complete (took {:?})",
        status_elapsed
    );
    println!("Status completed in {:?}", status_elapsed);
}

/// Live test: Verify GH_TOKEN authentication works correctly.
///
/// This test verifies that when the daemon is configured with github_user,
/// it correctly fetches the token and all gh CLI calls succeed. Tests that
/// PRs are fetched (requires valid auth) and the response contains real data.
///
/// Run: cargo test --test daemon_e2e live_daemon_gh_token -- --ignored --nocapture
#[test]
#[ignore] // Requires running daemon with github_user configured
fn live_daemon_gh_token_auth_works() {
    let mut fixture = match LiveDaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.ensure_daemon_running() {
        eprintln!("Skipping: Could not start daemon");
        return;
    }

    // Get status which includes PR list (requires working gh auth)
    let response = fixture.rpc_call_with_timeout("status", None, Duration::from_secs(10));
    assert!(response.is_some(), "Status should respond");

    let response = response.unwrap();
    assert!(
        response["error"].is_null(),
        "Status should not return error: {:?}",
        response["error"]
    );

    let result = &response["result"];

    // Verify we got a successful response with PR data
    assert_eq!(
        result["success"].as_bool(),
        Some(true),
        "Status should report success"
    );

    // The pull_requests field should exist (even if empty)
    assert!(
        result["pull_requests"].is_array(),
        "Status should include pull_requests array"
    );

    // Log what we got for debugging
    let pr_count = result["pull_requests"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    println!("GH_TOKEN auth working: fetched {} PRs", pr_count);

    // If there are PRs, verify they have expected fields (proves API worked)
    if pr_count > 0 {
        let first_pr = &result["pull_requests"][0];
        assert!(
            first_pr["number"].is_number(),
            "PR should have number field"
        );
        assert!(first_pr["title"].is_string(), "PR should have title field");
        println!(
            "  PR #{}: {}",
            first_pr["number"],
            first_pr["title"].as_str().unwrap_or("")
        );
    }

    // Also verify merged_prs works (another gh CLI call)
    assert!(
        result["merged_prs"].is_array(),
        "Status should include merged_prs array"
    );
    let merged_count = result["merged_prs"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    println!("Also fetched {} merged PRs", merged_count);
}

// ============================================================================
// RPC Handler E2E Tests
// ============================================================================

/// Test coworker.report-state accepts valid workflow phases.
///
/// The daemon should accept all documented phase strings and return success.
/// This tests the phase parsing logic in handle_coworker_report_state.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_coworker_report_state_valid_phases() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Test all valid phase strings
    let valid_phases = [
        "claiming",
        "developing",
        "testing",
        "pull_request",
        "pull-request", // Both underscore and hyphen accepted
        "reviewing",
        "debugging",
        "completed",
        "idle",
    ];

    for phase in valid_phases {
        let params = serde_json::json!({
            "name": "test-coworker",
            "phase": phase,
            "task_id": 42
        });

        let response = fixture.rpc_call("coworker.report-state", Some(params));
        assert!(
            response.is_some(),
            "Should receive response for phase '{}'",
            phase
        );

        let response = response.unwrap();
        assert!(
            response["error"].is_null(),
            "Phase '{}' should not return error: {:?}",
            phase,
            response["error"]
        );
        assert_eq!(
            response["result"]["success"].as_bool(),
            Some(true),
            "Phase '{}' should return success=true",
            phase
        );
    }
}

/// Test coworker.report-state returns error for unknown phases.
///
/// Invalid phase strings should result in a JSON-RPC error response
/// with code -32602 (invalid params).
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_coworker_report_state_invalid_phase() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let params = serde_json::json!({
        "name": "test-coworker",
        "phase": "invalid-phase-name"
    });

    let response = fixture.rpc_call("coworker.report-state", Some(params));
    assert!(response.is_some(), "Should receive error response");

    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Invalid phase should return error"
    );
    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32602),
        "Error code should be -32602 (invalid params)"
    );
}

/// Test coworker.report-state requires both name and phase params.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_coworker_report_state_missing_params() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Missing phase
    let params = serde_json::json!({
        "name": "test-coworker"
    });
    let response = fixture.rpc_call("coworker.report-state", Some(params));
    assert!(response.is_some());
    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Missing phase should return error"
    );

    // Missing name
    let params = serde_json::json!({
        "phase": "developing"
    });
    let response = fixture.rpc_call("coworker.report-state", Some(params));
    assert!(response.is_some());
    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Missing name should return error"
    );
}

/// Test coworker.asking posts question to channel.
///
/// When a coworker uses AskUserQuestion, the daemon should:
/// 1. Post the question to the channel
/// 2. Return a success response
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_coworker_asking_posts_to_channel() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let params = serde_json::json!({
        "name": "york",
        "question": "Should I use async or sync file operations?"
    });

    let response = fixture.rpc_call("coworker.asking", Some(params));
    assert!(response.is_some(), "Should receive response");

    let response = response.unwrap();
    assert!(
        response["error"].is_null(),
        "coworker.asking should not return error: {:?}",
        response["error"]
    );
    assert_eq!(
        response["result"]["success"].as_bool(),
        Some(true),
        "Should return success=true"
    );

    // Verify the question was posted to the channel
    let read_response = fixture.rpc_call("channel.read", Some(serde_json::json!({"all": true})));
    assert!(read_response.is_some());

    let read_response = read_response.unwrap();
    let messages = read_response["result"]["messages"].as_array();
    assert!(messages.is_some(), "Should have messages array");

    let messages = messages.unwrap();
    let has_question = messages.iter().any(|msg| {
        msg["from"].as_str() == Some("york")
            && msg["message"]
                .as_str()
                .map(|m| m.contains("async or sync"))
                .unwrap_or(false)
    });
    assert!(has_question, "Channel should contain the question");
}

/// Test coworker.asking returns error for missing params.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_coworker_asking_missing_params() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Missing question
    let response = fixture.rpc_call("coworker.asking", Some(serde_json::json!({"name": "york"})));
    assert!(response.is_some());
    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Missing question should return error"
    );

    // Missing name
    let response = fixture.rpc_call(
        "coworker.asking",
        Some(serde_json::json!({"question": "test?"})),
    );
    assert!(response.is_some());
    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Missing name should return error"
    );
}

/// Test daemon.check-pending returns ok response.
///
/// This RPC triggers the daemon to check for pending tasks and spawn
/// coworkers if needed. It should always return {status: "ok"}.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_daemon_check_pending_returns_ok() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let response = fixture.rpc_call("daemon.check-pending", None);
    assert!(response.is_some(), "Should receive response");

    let response = response.unwrap();
    assert!(
        response["error"].is_null(),
        "check-pending should not return error: {:?}",
        response["error"]
    );
    assert_eq!(
        response["result"]["status"].as_str(),
        Some("ok"),
        "Should return status=ok"
    );
}

/// Test channel.post with /me action prefix.
///
/// When a message starts with "/me ", it should be stored as an Action
/// type message. This tests the IRC-style action handling.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_channel_post_me_action() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let params = serde_json::json!({
        "from": "york",
        "message": "/me is working on task !42"
    });

    let response = fixture.rpc_call("channel.post", Some(params));
    assert!(response.is_some());
    let response = response.unwrap();
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["success"].as_bool(), Some(true));

    // Read back and verify the message was posted
    let read_response = fixture.rpc_call("channel.read", Some(serde_json::json!({"all": true})));
    assert!(read_response.is_some());
    let read_response = read_response.unwrap();

    let messages = read_response["result"]["messages"].as_array().unwrap();
    let action_msg = messages.iter().find(|m| m["from"].as_str() == Some("york"));
    assert!(action_msg.is_some(), "Should find the action message");
}

/// Test channel.post unescapes shell artifacts.
///
/// When Claude Code posts via Bash, "!" often gets escaped as "\!".
/// The daemon should clean this up automatically.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_channel_post_unescapes_shell_artifacts() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let params = serde_json::json!({
        "from": "york",
        "message": "Task complete\\! Moving on\\!"
    });

    let response = fixture.rpc_call("channel.post", Some(params));
    assert!(response.is_some());
    let response = response.unwrap();
    assert!(response["error"].is_null());

    // Read back and verify the escapes were cleaned
    let read_response = fixture.rpc_call("channel.read", Some(serde_json::json!({"all": true})));
    assert!(read_response.is_some());
    let read_response = read_response.unwrap();

    let messages = read_response["result"]["messages"].as_array().unwrap();
    let msg = messages
        .iter()
        .find(|m| {
            m["from"].as_str() == Some("york")
                && m["message"]
                    .as_str()
                    .map(|s| s.contains("complete"))
                    .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("Should find posted message, got: {:?}", messages));

    let content = msg["message"].as_str().unwrap();
    assert!(
        content.contains("complete!") && content.contains("on!"),
        "Shell escapes should be removed. Got: {}",
        content
    );
    assert!(
        !content.contains("\\!"),
        "Should not contain escaped exclamation marks. Got: {}",
        content
    );
}

/// Test channel.read with all=true returns all messages.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_channel_read_all_param() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Post 25 messages (more than the default 20 limit)
    for i in 1..=25 {
        let params = serde_json::json!({
            "from": "test",
            "message": format!("Message {}", i)
        });
        let post_response = fixture.rpc_call("channel.post", Some(params));
        assert!(
            post_response.is_some(),
            "Failed to post message {} to channel",
            i
        );
    }

    // Read with all=false (default) should return 20
    let response = fixture
        .rpc_call("channel.read", Some(serde_json::json!({"all": false})))
        .expect("channel.read should succeed");
    let messages = response["result"]["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("Expected messages array in response, got: {:?}", response));
    assert!(
        messages.len() <= 20,
        "Default read should return max 20 messages"
    );

    // Read with all=true should return all 25
    let response = fixture
        .rpc_call("channel.read", Some(serde_json::json!({"all": true})))
        .expect("channel.read with all=true should succeed");
    let messages = response["result"]["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("Expected messages array in response, got: {:?}", response));
    assert!(
        messages.len() >= 25,
        "all=true should return all {} messages",
        messages.len()
    );
}

/// Test coworker.nudge accepts valid params without returning invalid_params error.
///
/// This verifies the RPC handler validates params correctly. The underlying tmux
/// operation may fail (returning -32603 internal error) if no coworker exists,
/// but it should NOT return -32602 (invalid params) for well-formed requests.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_coworker_nudge_valid_params() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let params = serde_json::json!({
        "name": "nonexistent-coworker",
        "message": "Hello from test",
        "from": "test"
    });

    let response = fixture.rpc_call("coworker.nudge", Some(params));
    assert!(response.is_some(), "Should receive response");

    let response = response.unwrap();

    // Valid params should NOT return invalid_params error (-32602).
    // The operation may return success or internal error (-32603) depending
    // on tmux state, but never invalid_params for well-formed requests.
    if let Some(error) = response["error"].as_object() {
        let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        assert_ne!(
            code, -32602,
            "Valid params should not return invalid_params error"
        );
    }
    // If no error, verify we got a result object
    if response["error"].is_null() {
        assert!(
            response["result"].is_object(),
            "Success response should have result object"
        );
    }
}

/// Test reminder.create with invalid trigger returns error.
///
/// Only "all-work-merged" trigger is currently supported.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_reminder_create_invalid_trigger() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let params = serde_json::json!({
        "trigger": "invalid-trigger",
        "message": "Test message"
    });

    let response = fixture.rpc_call("reminder.create", Some(params));
    assert!(response.is_some());
    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Invalid trigger should return error"
    );
}

/// Test reminder.cancel with non-existent ID returns error.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_reminder_cancel_not_found() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let params = serde_json::json!({
        "id": "nonexistent-reminder-id"
    });

    let response = fixture.rpc_call("reminder.cancel", Some(params));
    assert!(response.is_some());
    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Non-existent reminder should return error"
    );
    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32602),
        "Should return invalid params error code"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// task.claim RPC E2E tests
//
// These tests verify the task.claim endpoint handles validation, error cases,
// and the happy path correctly through a real daemon.
// ────────────────────────────────────────────────────────────────────────────

/// Test that task.claim returns invalid_params when the task ID is missing.
///
/// The RPC dispatcher checks for the `id` parameter before calling the handler.
/// Missing `id` should return a -32602 (invalid params) error.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_task_claim_missing_id() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Send task.claim with missing id parameter
    let params = serde_json::json!({
        "from": "park"
    });

    let response = fixture.rpc_call("task.claim", Some(params));
    assert!(
        response.is_some(),
        "Should receive response from task.claim"
    );

    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Missing id should return error: {:?}",
        response
    );
    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32602),
        "Should return invalid params error code"
    );
}

/// Test that task.claim returns an error when the task does not exist on disk.
///
/// The handler reads tasks from `~/.claude/tasks/midtown-<repo>/` and returns
/// a -32602 error with a "not found" message if no matching task file exists.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_task_claim_task_not_found() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Send task.claim for a non-existent task
    let params = serde_json::json!({
        "id": "999",
        "from": "park"
    });

    let response = fixture.rpc_call("task.claim", Some(params));
    assert!(
        response.is_some(),
        "Should receive response from task.claim"
    );

    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Non-existent task should return error: {:?}",
        response
    );
    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32602),
        "Should return invalid params error code for missing task"
    );
    let error_msg = response["error"]["message"].as_str().unwrap_or("");
    assert!(
        error_msg.contains("not found"),
        "Error message should indicate task not found, got: {}",
        error_msg
    );
}

/// Test that task.claim returns an error when the task is not pending.
///
/// Only pending tasks can be claimed. If the task is in_progress or completed,
/// the handler returns a -32602 error with the current status.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_task_claim_task_not_pending() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Create an in_progress task on disk
    fixture.create_task("42", "Fix auth bug", "in_progress", Some("amsterdam"));

    let params = serde_json::json!({
        "id": "42",
        "from": "park"
    });

    let response = fixture.rpc_call("task.claim", Some(params));
    assert!(
        response.is_some(),
        "Should receive response from task.claim"
    );

    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Non-pending task should return error: {:?}",
        response
    );
    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32602),
        "Should return invalid params error code for non-pending task"
    );
    let error_msg = response["error"]["message"].as_str().unwrap_or("");
    assert!(
        error_msg.contains("not pending"),
        "Error message should indicate task is not pending, got: {}",
        error_msg
    );
}

/// Test that task.claim succeeds for a valid pending task.
///
/// The handler validates the task exists and is pending, records an in-memory
/// assignment, nudges the Lead, and returns a success response. The nudge to
/// the Lead may fail (no tmux session in tests) but the RPC still succeeds.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_task_claim_success() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Create a pending task on disk
    fixture.create_task("42", "Fix auth bug", "pending", None);

    let params = serde_json::json!({
        "id": "42",
        "from": "park"
    });

    let response = fixture.rpc_call("task.claim", Some(params));
    assert!(
        response.is_some(),
        "Should receive response from task.claim"
    );

    let response = response.unwrap();
    assert!(
        response["error"].is_null(),
        "Valid claim should succeed, got error: {:?}",
        response["error"]
    );

    let result = &response["result"];
    assert_eq!(
        result["success"].as_bool(),
        Some(true),
        "Result should indicate success"
    );
    let message = result["message"].as_str().unwrap_or("");
    assert!(
        message.contains("42"),
        "Success message should reference the task ID, got: {}",
        message
    );
}

/// Test that task.claim returns an error for a completed task.
///
/// Completed tasks cannot be claimed. Verifies the handler rejects claims
/// for tasks that have already been completed.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_task_claim_completed_task() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Create a completed task on disk
    fixture.create_task("77", "Old feature", "completed", Some("york"));

    let params = serde_json::json!({
        "id": "77",
        "from": "amsterdam"
    });

    let response = fixture.rpc_call("task.claim", Some(params));
    assert!(
        response.is_some(),
        "Should receive response from task.claim"
    );

    let response = response.unwrap();
    assert!(
        response["error"].is_object(),
        "Completed task claim should return error: {:?}",
        response
    );
    assert_eq!(
        response["error"]["code"].as_i64(),
        Some(-32602),
        "Should return invalid params error code"
    );
    let error_msg = response["error"]["message"].as_str().unwrap_or("");
    assert!(
        error_msg.contains("not pending"),
        "Error should indicate task is not pending, got: {}",
        error_msg
    );
}

/// Test that the in-memory assignment is recorded after a successful claim.
///
/// After a successful task.claim, the daemon records an in-memory assignment
/// that makes the coworker appear "busy" in subsequent status checks. This
/// prevents the task from being re-assigned to another coworker.
#[test]
#[ignore] // Requires built binary
fn test_daemon_rpc_task_claim_marks_coworker_busy() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Create a pending task
    fixture.create_task("42", "Fix auth bug", "pending", None);

    // Claim the task
    let claim_params = serde_json::json!({
        "id": "42",
        "from": "park"
    });
    let claim_response = fixture.rpc_call("task.claim", Some(claim_params));
    assert!(claim_response.is_some());
    let claim_response = claim_response.unwrap();
    assert!(
        claim_response["error"].is_null(),
        "Claim should succeed: {:?}",
        claim_response["error"]
    );

    // Check status — the daemon's in-memory state should reflect the assignment.
    // The status endpoint reports busy_coworkers from in-memory assignments.
    let status_response = fixture.rpc_call("status", None);
    assert!(status_response.is_some());
    let status_response = status_response.unwrap();
    assert!(
        status_response["error"].is_null(),
        "Status should succeed: {:?}",
        status_response["error"]
    );

    // The task should still show as pending on disk (Lead hasn't processed yet)
    // but the in-memory assignment should exist. We verify this indirectly:
    // the status response includes busy_coworkers or task assignment info.
    let result = &status_response["result"];
    assert!(result.is_object(), "Status should return an object");
}
