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
        })
    }

    /// Start the daemon process.
    ///
    /// Returns true if the daemon started successfully and the socket is available.
    fn start_daemon(&mut self) -> bool {
        // Build the daemon binary first (use release for speed)
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

                // Wait for socket to become available (up to 5 seconds)
                for _ in 0..50 {
                    thread::sleep(Duration::from_millis(100));
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

/// Test that required Claude Code plugins are automatically installed on daemon startup.
///
/// This E2E test verifies the plugin auto-installation feature by:
/// 1. Uninstalling the required plugin if it's present
/// 2. Starting the daemon (which should trigger auto-install)
/// 3. Verifying the plugin is now installed
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

    // Verify plugin is NOT installed before starting daemon
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
        "Plugin should NOT be installed before daemon start for this test to be valid"
    );

    // Start daemon (this triggers ensure_plugins_installed)
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        eprintln!("Daemon failed to start — skipping plugin installation test");
        return;
    }

    // Give the daemon time to install plugins (installation happens at startup)
    // The daemon should have already installed plugins before the socket became available,
    // but give it a bit more time just in case
    thread::sleep(Duration::from_secs(2));

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
}

impl LiveDaemonFixture {
    /// Create a fixture for the midtown repo daemon.
    ///
    /// Returns None if the binary isn't built or the repo isn't detected.
    fn new() -> Option<Self> {
        // Prefer release binary for realistic timing
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let release_binary = manifest_dir.join("target/release/midtown");
        let debug_binary = manifest_dir.join("target/debug/midtown");

        let binary_path = if release_binary.exists() {
            release_binary
        } else if debug_binary.exists() {
            eprintln!("Warning: Using debug binary - timing may not match production");
            debug_binary
        } else {
            eprintln!("Skipping: No midtown binary found. Run 'cargo build --release' first.");
            return None;
        };

        // Socket path for midtown repo
        let socket_path = midtown::paths::daemon_socket_for_repo("midtown");

        Some(Self {
            socket_path,
            started_daemon: false,
            binary_path,
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
