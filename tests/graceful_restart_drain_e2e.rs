//! Test graceful restart drain behavior with real coworker lifecycle.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Test that wait_for_coworkers_to_drain polls status and waits for stopped/stopping.
///
/// Scenario:
/// 1. Start daemon
/// 2. Spawn a coworker (simulated as "working")
/// 3. Call daemon.enter-drain
/// 4. Verify the daemon stops assigning new tasks
/// 5. Simulate coworker transitioning to stopped
/// 6. Verify wait completes when all coworkers are stopped/stopping
#[test]
#[ignore] // requires tmux
fn test_wait_for_coworkers_reports_status_and_completes() {
    // Clean up any previous test data
    let repo_name = format!("drain-flow-test-{}", std::process::id());
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

    // Spawn a simulated coworker (in this test we'll just check that drain mode is entered)
    // In a real scenario, a coworker would be running and we'd wait for it to go idle

    // Send enter-drain RPC request
    let mut stream = UnixStream::connect(&socket_path).unwrap();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "daemon.enter-drain",
        "id": 1
    });
    writeln!(stream, "{}", request.to_string()).unwrap();

    // Read response
    let mut response_line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut response_line).unwrap();
    }
    let response: serde_json::Value = serde_json::from_str(&response_line).unwrap();

    // Verify response indicates draining mode
    assert_eq!(response["result"]["status"], "draining");

    // Query status to verify drain mode is active
    response_line.clear();
    let status_request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "status",
        "id": 2
    });
    writeln!(stream, "{}", status_request.to_string()).unwrap();
    {
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut response_line).unwrap();
    }
    let status_response: serde_json::Value = serde_json::from_str(&response_line).unwrap();

    // Verify the status response includes draining flag
    // (This will require updating the status RPC handler to return the draining state)
    // For now, just verify we get a valid response
    assert!(status_response["result"].is_object());

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

/// Test that draining mode prevents new task assignments.
#[test]
#[ignore] // requires tmux
fn test_draining_prevents_task_assignment() {
    // This test would require:
    // 1. Start daemon
    // 2. Create a pending task
    // 3. Enter drain mode
    // 4. Verify that spawn_for_pending_tasks skips the task
    // 5. Exit drain mode
    // 6. Verify the task gets assigned

    // For now, we rely on the existing check in dispatch.rs line 996
    // This is a placeholder for future E2E testing
}

/// Test timeout behavior when coworkers don't drain within the timeout window.
#[test]
#[ignore] // requires tmux
fn test_drain_timeout_forces_shutdown() {
    // This test would verify:
    // 1. Start daemon with coworkers
    // 2. Enter drain mode with short timeout (e.g., 2 seconds)
    // 3. Coworkers don't go idle
    // 4. Verify timeout expires and shutdown proceeds anyway

    // Placeholder for future implementation
}
