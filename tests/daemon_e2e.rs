//! End-to-end tests for daemon startup and API endpoints.
//!
//! These tests verify the daemon can start successfully and respond to RPC
//! requests. They're smoke tests to catch regressions in daemon lifecycle.
//!
//! Run with `cargo test -- --ignored daemon` as these spawn real processes.

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

/// Test fixture for daemon e2e tests.
///
/// Creates an isolated environment with a fake git repo and manages
/// daemon lifecycle.
#[allow(dead_code)]
struct DaemonFixture {
    /// Temporary directory containing the test repo
    temp_dir: PathBuf,
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

        let midtown_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".midtown")
            .join(&repo_name);
        let pid_path = midtown_dir.join("daemon.pid");

        // Ensure parent directories exist
        if let Some(parent) = socket_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Some(parent) = pid_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        Some(Self {
            temp_dir,
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
        let mut stream = self.connect()?;

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
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        self.stop_daemon();

        // Clean up socket and pid files
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.pid_path);

        // Clean up socket parent directory if empty
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir(parent);
        }

        // Clean up pid parent directory if empty
        if let Some(parent) = self.pid_path.parent() {
            let _ = fs::remove_dir(parent);
        }

        // Clean up temp directory
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

/// Test that the daemon starts and creates a socket.
#[test]
#[ignore] // Requires built binary
fn test_daemon_starts_and_creates_socket() {
    let mut fixture = match DaemonFixture::new() {
        Some(f) => f,
        None => {
            eprintln!("Skipping test: could not create test fixture");
            return;
        }
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
        None => {
            eprintln!("Skipping test: could not create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping test: daemon did not start");
        return;
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
        None => {
            eprintln!("Skipping test: could not create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping test: daemon did not start");
        return;
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
        None => {
            eprintln!("Skipping test: could not create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping test: daemon did not start");
        return;
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
        None => {
            eprintln!("Skipping test: could not create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping test: daemon did not start");
        return;
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
        None => {
            eprintln!("Skipping test: could not create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping test: daemon did not start");
        return;
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
        None => {
            eprintln!("Skipping test: could not create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping test: daemon did not start");
        return;
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
        None => {
            eprintln!("Skipping test: could not create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping test: daemon did not start");
        return;
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
        None => {
            eprintln!("Skipping test: could not create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping test: daemon did not start");
        return;
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
        None => {
            eprintln!("Skipping test: could not create test fixture");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping test: daemon did not start");
        return;
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
