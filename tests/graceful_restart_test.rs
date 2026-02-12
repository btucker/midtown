//! Test for graceful restart drain mode.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Find the midtown binary using the same candidate path pattern as other E2E tests.
/// Returns None if no binary is found (test should be skipped).
fn find_binary() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest_dir.join("target/release/midtown"),
        manifest_dir.join("target/debug/midtown"),
        // cargo-llvm-cov uses a separate target directory for instrumented builds
        manifest_dir.join("target/llvm-cov-target/debug/midtown"),
    ];

    candidates.iter().find(|p| p.exists()).cloned()
}

/// Test that daemon.enter-drain RPC sets the draining flag and that a daemon
/// with no coworkers drains immediately (no tasks to wait for).
#[test]
#[ignore] // requires built binary
fn test_enter_drain_mode() {
    let binary_path = match find_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping: No midtown binary found. Run `cargo build` first.");
            return;
        }
    };

    // Clean up any previous test data
    let repo_name = format!("drain-test-{}", std::process::id());
    let temp_dir = std::env::temp_dir().join(&repo_name);
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // Initialize a git repository
    let status = Command::new("git")
        .args(["init"])
        .current_dir(&temp_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());

    // Compute socket path
    let state_dir = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap().join(".local").join("state"));
    let socket_path = state_dir
        .join("midtown")
        .join(&repo_name)
        .join("daemon.sock");

    // Remove stale socket
    let _ = fs::remove_file(&socket_path);

    // Ensure parent directory exists
    if let Some(parent) = socket_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // Start the daemon
    let mut daemon = Command::new(&binary_path)
        .arg("daemon")
        .arg("--workdir")
        .arg(&temp_dir)
        .current_dir(&temp_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("MIDTOWN_WEBHOOK_PORT", "0")
        .env("MIDTOWN_CHAT_MONITOR", "0")
        .spawn()
        .unwrap();

    // Wait for socket to become available
    let mut socket_ready = false;
    for _ in 0..300 {
        thread::sleep(Duration::from_millis(200));
        if socket_path.exists() && UnixStream::connect(&socket_path).is_ok() {
            socket_ready = true;
            break;
        }
    }
    assert!(socket_ready, "Daemon socket did not become available");

    // Send enter-drain RPC request
    let mut stream = UnixStream::connect(&socket_path).unwrap();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "daemon.enter-drain",
        "id": 1
    });
    writeln!(stream, "{}", request).unwrap();

    // Read response
    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&response_line).unwrap();

    // Verify response indicates draining mode
    assert_eq!(response["result"]["status"], "draining");

    // Query daemon status to verify no coworkers are running (drain should be immediate)
    let mut stream2 = UnixStream::connect(&socket_path).unwrap();
    let status_request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "daemon.status",
        "id": 2
    });
    writeln!(stream2, "{}", status_request).unwrap();

    let mut reader2 = BufReader::new(stream2);
    let mut status_line = String::new();
    reader2.read_line(&mut status_line).unwrap();
    let status_resp: serde_json::Value = serde_json::from_str(&status_line).unwrap();

    // With no coworkers spawned, the coworkers list should be empty
    let coworkers = status_resp["result"]["coworkers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        coworkers.is_empty(),
        "Expected no coworkers in drain test, got: {:?}",
        coworkers
    );

    // Kill daemon
    let _ = daemon.kill();
    let _ = daemon.wait();

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
    let project_dir = dirs::home_dir()
        .unwrap()
        .join(".midtown")
        .join("projects")
        .join(&repo_name);
    let _ = fs::remove_dir_all(&project_dir);
    if let Some(parent) = socket_path.parent() {
        let _ = fs::remove_dir_all(parent);
    }
}
