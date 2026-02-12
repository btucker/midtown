//! Test for graceful restart drain mode.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Test that daemon.enter-drain RPC sets the draining flag.
#[test]
#[ignore] // requires tmux
fn test_enter_drain_mode() {
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

    // Build the binary
    let build_result = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(build_result.success());

    let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("midtown");

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
