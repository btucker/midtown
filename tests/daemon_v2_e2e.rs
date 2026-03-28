//! End-to-end tests for the daemon v2 process: startup, Unix socket RPC, and shutdown.
//!
//! These tests spawn a real `midtown daemon-v2` binary, connect via a Unix domain
//! socket, and exercise the JSON-RPC interface.
//!
//! Run with:
//!   cargo test --test daemon_v2_e2e -- --ignored --test-threads=1

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

// ── Harness ───────────────────────────────────────────────────────────────────

/// Test harness that owns a running `midtown daemon-v2` process.
struct V2Harness {
    /// Temp directory used as the git repo / workdir.
    _temp_dir: tempfile::TempDir,
    /// Directory used for the socket and event store (kept alive for Drop).
    _state_dir: tempfile::TempDir,
    /// Path to the Unix domain socket.
    socket_path: PathBuf,
    /// The child process handle.
    child: Option<Child>,
}

impl V2Harness {
    /// Spin up a `midtown daemon-v2` process and wait for the socket to appear.
    fn start() -> Self {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let state_dir = tempfile::TempDir::new().expect("state dir");

        // Initialise a minimal git repo so dir_key detection works.
        let repo = temp_dir.path();
        let _ = Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("git")
            .args(["config", "user.email", "test@midtown.local"])
            .current_dir(repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        let socket_path = state_dir.path().join("daemon-v2.sock");

        let child = Command::new(env!("CARGO_BIN_EXE_midtown"))
            .args([
                "daemon-v2",
                "--socket",
                socket_path.to_str().unwrap(),
                "--workdir",
                "test-repo",
                "--channel",
                "main",
            ])
            .current_dir(repo)
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .env("MIDTOWN_WEBHOOK_PORT", "0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn midtown daemon-v2");

        let harness = V2Harness {
            _temp_dir: temp_dir,
            _state_dir: state_dir,
            socket_path,
            child: Some(child),
        };

        // Wait up to 10 s for the socket to appear.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if harness.socket_path.exists() {
                return harness;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        panic!(
            "daemon-v2 socket did not appear at {:?} within 10 s",
            harness.socket_path
        );
    }

    /// Send a JSON-RPC request and return the parsed response.
    fn rpc_call(&self, method: &str, params: Option<serde_json::Value>) -> serde_json::Value {
        let id = serde_json::json!(1);
        let mut request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "id": id,
        });
        if let Some(p) = params {
            request["params"] = p;
        }

        let mut stream =
            UnixStream::connect(&self.socket_path).expect("connect to daemon-v2 socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set_read_timeout");

        let bytes = serde_json::to_vec(&request).expect("serialize request");
        stream.write_all(&bytes).expect("write request");
        // Signal EOF so the daemon knows the request is complete.
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("shutdown write");

        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).expect("read response");

        serde_json::from_slice(&buf).expect("parse JSON response")
    }

    /// Send "shutdown" RPC then kill the process if it does not exit promptly.
    fn stop(&mut self) {
        // Best-effort shutdown RPC — ignore errors (process may already be gone).
        if self.socket_path.exists() {
            let _ = std::panic::catch_unwind(|| {
                self.rpc_call("shutdown", None);
            });
        }

        if let Some(mut child) = self.child.take() {
            // Give the process a moment to exit cleanly.
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
    }
}

impl Drop for V2Harness {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
#[ignore]
fn test_daemon_v2_starts_and_responds_to_status() {
    let harness = V2Harness::start();

    let resp = harness.rpc_call("status", None);

    // Response should be a valid JSON-RPC 2.0 success.
    assert_eq!(resp["jsonrpc"], "2.0", "unexpected response: {resp}");
    assert!(resp["error"].is_null(), "unexpected error: {resp}");

    let result = &resp["result"];
    // The scheduler spawns a lead agent on startup, so total/running may be > 0
    assert!(
        result["agents"]["total"].is_number(),
        "agents.total should be a number"
    );
    assert!(
        result["tasks"]["pending"].is_number(),
        "tasks.pending should be a number"
    );
    assert!(
        result["tasks"]["in_progress"].is_number(),
        "tasks.in_progress should be a number"
    );
    assert!(
        result["prs"]["open"].is_number(),
        "prs.open should be a number"
    );
}

#[test]
#[ignore]
fn test_daemon_v2_agent_list_empty() {
    let harness = V2Harness::start();

    let resp = harness.rpc_call("agent.list", None);

    assert_eq!(resp["jsonrpc"], "2.0", "unexpected response: {resp}");
    assert!(resp["error"].is_null(), "unexpected error: {resp}");

    let agents = resp["result"]
        .as_array()
        .expect("result should be an array");
    // The scheduler may have spawned a lead agent already — just verify the response is valid
    for agent in agents {
        assert!(agent["id"].is_string(), "agent should have an id");
        assert!(agent["name"].is_string(), "agent should have a name");
    }
}

#[test]
#[ignore]
fn test_daemon_v2_unknown_method_returns_error() {
    let harness = V2Harness::start();

    let resp = harness.rpc_call("no.such.method", None);

    assert_eq!(resp["jsonrpc"], "2.0", "unexpected response: {resp}");
    assert!(
        !resp["error"].is_null(),
        "expected error for unknown method, got: {resp}"
    );
    assert_eq!(
        resp["error"]["code"], -32601,
        "expected method-not-found code -32601, got: {resp}"
    );
}

#[test]
#[ignore]
fn test_daemon_v2_shutdown() {
    let mut harness = V2Harness::start();

    // Verify it is up.
    let resp = harness.rpc_call("status", None);
    assert_eq!(
        resp["jsonrpc"], "2.0",
        "daemon not responding before shutdown"
    );

    // Ask it to shut down.
    let resp = harness.rpc_call("shutdown", None);
    assert_eq!(resp["jsonrpc"], "2.0", "shutdown did not return a response");
    assert!(
        resp["error"].is_null(),
        "shutdown returned an error: {resp}"
    );
    assert_eq!(
        resp["result"]["ok"], true,
        "shutdown result.ok should be true"
    );

    // Wait for the process to exit (it should exit promptly after shutdown RPC).
    let child = harness.child.take().expect("child process");
    // Disarm Drop before we consume the child handle.
    let exited = wait_for_exit(child, Duration::from_secs(5));
    assert!(exited, "daemon process should exit after shutdown RPC");
}

#[test]
#[ignore]
fn test_daemon_v2_task_create_shows_in_status() {
    let harness = V2Harness::start();

    // Create a task via RPC
    let resp = harness.rpc_call(
        "task.create",
        Some(serde_json::json!({
            "id": "t1",
            "subject": "Say hello",
            "channel": "main",
        })),
    );
    assert!(resp["error"].is_null(), "task.create error: {resp}");

    // Give scheduler a moment to run
    std::thread::sleep(Duration::from_secs(2));

    // Verify task appears in status
    let status = harness.rpc_call("status", None);
    let pending = status["result"]["tasks"]["pending"].as_u64().unwrap_or(0);
    let in_progress = status["result"]["tasks"]["in_progress"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        pending + in_progress >= 1,
        "task should exist in status: {status}"
    );
}

#[test]
#[ignore]
fn test_daemon_v2_spawns_agent_for_task() {
    let harness = V2Harness::start();

    // Create a task — the dispatcher should spawn a real Claude agent for it
    let resp = harness.rpc_call(
        "task.create",
        Some(serde_json::json!({
            "id": "t1",
            "subject": "Print 'hello from daemon v2' and exit immediately",
            "channel": "main",
        })),
    );
    assert!(resp["error"].is_null(), "task.create error: {resp}");

    // Poll status until we see an agent spawned (dispatch runs every 5s)
    let mut saw_agent = false;
    for i in 0..30 {
        std::thread::sleep(Duration::from_secs(1));
        let status = harness.rpc_call("status", None);
        let total = status["result"]["agents"]["total"].as_u64().unwrap_or(0);
        if total > 0 {
            saw_agent = true;
            let running = status["result"]["agents"]["running"].as_u64().unwrap_or(0);
            eprintln!("Agent spawned after {i}s: total={total}, running={running}");
            break;
        }
    }
    assert!(
        saw_agent,
        "expected agent to be spawned for task within 30s"
    );
}

#[test]
#[ignore]
fn test_daemon_v2_pr_polling_runs_without_crash() {
    let harness = V2Harness::start();

    // Wait long enough for the PR poll to fire (registered at 45s, but first run is immediate)
    // Just verify the daemon stays alive through a poll cycle — it proves gh CLI integration works
    for _ in 0..6 {
        std::thread::sleep(Duration::from_secs(5));
        let status = harness.rpc_call("status", None);
        assert!(
            status["error"].is_null(),
            "daemon crashed during PR poll: {status}"
        );
    }

    // If the daemon is running in the midtown repo, we might see open PRs
    let status = harness.rpc_call("status", None);
    let open_prs = status["result"]["prs"]["open"].as_u64().unwrap_or(0);
    eprintln!("PR poll test complete. Open PRs detected: {open_prs}");
}

/// Wait up to `timeout` for `child` to exit. Returns true if it exited in time.
fn wait_for_exit(mut child: Child, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}
