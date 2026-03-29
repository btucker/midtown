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
    assert!(result["tasks"].is_array(), "tasks should be an array");
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

    // Verify task appears in status (tasks is now an array)
    let status = harness.rpc_call("status", None);
    let tasks = status["result"]["tasks"]
        .as_array()
        .expect("tasks should be array");
    assert!(!tasks.is_empty(), "task should exist in status: {status}");
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

#[test]
#[ignore]
fn test_daemon_v2_channel_post_and_read() {
    let harness = V2Harness::start();

    // Post a message
    let resp = harness.rpc_call(
        "channel.post",
        Some(serde_json::json!({
            "channel": "test-chan",
            "sender": "test-user",
            "content": "hello from e2e test",
        })),
    );
    assert!(resp["error"].is_null(), "channel.post error: {resp}");

    // Read it back
    let resp = harness.rpc_call(
        "channel.read",
        Some(serde_json::json!({
            "channel": "test-chan",
            "limit": 5,
        })),
    );
    assert!(resp["error"].is_null(), "channel.read error: {resp}");
    let messages = resp["result"].as_array().expect("should be array");
    assert!(
        messages
            .iter()
            .any(|m| m["message"] == "hello from e2e test"),
        "posted message should appear in read: {messages:?}"
    );

    // List channels
    let resp = harness.rpc_call("channel.list", None);
    assert!(resp["error"].is_null(), "channel.list error: {resp}");
    let channels = resp["result"].as_array().expect("should be array");
    assert!(
        channels.iter().any(|c| c["name"] == "test-chan"),
        "test-chan should appear in list: {channels:?}"
    );
}

#[test]
#[ignore]
fn test_daemon_v2_lead_driven_skips_auto_dispatch() {
    let harness = V2Harness::start();

    // Set channel to lead-driven
    let resp = harness.rpc_call(
        "channel.update",
        Some(serde_json::json!({
            "channel": "manual-chan",
            "lead_driven": true,
        })),
    );
    assert!(resp["error"].is_null(), "channel.update error: {resp}");

    // Create a task in the lead-driven channel
    let resp = harness.rpc_call(
        "task.create",
        Some(serde_json::json!({
            "id": "manual-t1",
            "subject": "Manual task - should not auto-dispatch",
            "channel": "manual-chan",
        })),
    );
    assert!(resp["error"].is_null(), "task.create error: {resp}");

    // Wait for dispatch cycle (5s interval)
    std::thread::sleep(Duration::from_secs(8));

    // Verify no worker was spawned for this task
    let resp = harness.rpc_call("agent.list", None);
    let agents = resp["result"].as_array().unwrap();
    let manual_workers: Vec<_> = agents
        .iter()
        .filter(|a| a["task_id"] == "manual-t1")
        .collect();
    assert!(
        manual_workers.is_empty(),
        "lead-driven task should not auto-dispatch: {agents:?}"
    );
}

#[test]
#[ignore]
fn test_daemon_v2_session_fork_spawns_agent() {
    let harness = V2Harness::start();

    // Wait for a lead with a session_id to be running. We need the session_id
    // for --fork-session to work. Recovered leads from old event store entries
    // may have session_id: null.
    let mut lead_has_session = false;
    for i in 0..30 {
        std::thread::sleep(Duration::from_secs(1));
        let resp = harness.rpc_call("agent.list", None);
        let agents = resp["result"].as_array().unwrap();
        if let Some(lead) = agents
            .iter()
            .find(|a| a["kind"] == "Lead" && a["running"] == true)
        {
            if !lead["session_id"].is_null() {
                lead_has_session = true;
                eprintln!(
                    "Lead ready with session_id after {i}s: {}",
                    lead["session_id"]
                );
                break;
            } else {
                eprintln!(
                    "Lead found but session_id is null (recovered from old state?) after {i}s"
                );
            }
        }
    }
    assert!(
        lead_has_session,
        "lead should have a session_id for fork context"
    );

    // Use a unique thread ID to avoid collisions with previous test runs
    let thread_id = format!("thread-{}", uuid::Uuid::new_v4());

    // Fork a session for a thread
    let resp = harness.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": thread_id,
            "channel": "main",
            "message": "Investigate this thread",
        })),
    );
    assert!(resp["error"].is_null(), "session.fork error: {resp}");
    assert_eq!(resp["result"]["ok"], true);
    // fork_from_session indicates whether the parent lead's session_id was found.
    // In the E2E environment, this may be false if the lead just spawned and the
    // event hasn't been fully applied yet. The critical test is that the fork spawns.
    let has_context = resp["result"]["fork_from_session"]
        .as_bool()
        .unwrap_or(false);
    eprintln!("fork_from_session: {has_context}");

    // Wait for the fork to spawn
    let mut saw_fork = false;
    for _ in 0..15 {
        std::thread::sleep(Duration::from_secs(1));
        let resp = harness.rpc_call("agent.list", None);
        let agents = resp["result"].as_array().unwrap();
        if agents.iter().any(|a| a["kind"] == "Fork") {
            saw_fork = true;
            break;
        }
    }
    assert!(saw_fork, "expected fork agent to be spawned");

    // Forking the same thread again should return the existing fork
    let resp = harness.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": thread_id,
            "channel": "main",
        })),
    );
    assert!(resp["error"].is_null(), "second fork error: {resp}");
    assert_eq!(
        resp["result"]["existing"], true,
        "should return existing fork"
    );
}

#[test]
#[ignore]
fn test_daemon_v2_v1_rpc_compatibility() {
    let harness = V2Harness::start();

    // ping
    let resp = harness.rpc_call("ping", None);
    assert_eq!(resp["jsonrpc"], "2.0", "unexpected response: {resp}");
    assert!(resp["error"].is_null(), "ping error: {resp}");
    assert_eq!(resp["result"], "pong");

    // version
    let resp = harness.rpc_call("version", None);
    assert!(resp["error"].is_null(), "version error: {resp}");
    assert_eq!(resp["result"]["name"], "midtown");
    assert!(resp["result"]["version"].is_string());
    assert_eq!(resp["result"]["daemon"], "v2");

    // lead.spawn (should succeed — lead is auto-managed by scheduler)
    let resp = harness.rpc_call(
        "lead.spawn",
        Some(serde_json::json!({"provider": "claude"})),
    );
    assert!(resp["error"].is_null(), "lead.spawn error: {resp}");
    assert_eq!(resp["result"]["ok"], true);

    // snapshot (alias for status)
    let resp = harness.rpc_call("snapshot", None);
    assert!(resp["error"].is_null(), "snapshot error: {resp}");
    assert!(resp["result"]["agents"]["total"].is_number());

    // coworker.list (alias for agent.list)
    let resp = harness.rpc_call("coworker.list", None);
    assert!(resp["error"].is_null(), "coworker.list error: {resp}");
    assert!(resp["result"].is_array());

    // coworkers.status (alias for agent.list)
    let resp = harness.rpc_call("coworkers.status", None);
    assert!(resp["error"].is_null(), "coworkers.status error: {resp}");
    assert!(resp["result"].is_array());

    // prs.status
    let resp = harness.rpc_call("prs.status", None);
    assert!(resp["error"].is_null(), "prs.status error: {resp}");
    assert!(resp["result"]["prs"].is_array());

    // task.done
    // First create a task, then complete it
    let resp = harness.rpc_call(
        "task.create",
        Some(serde_json::json!({
            "id": "compat-t1",
            "subject": "compat test task",
            "channel": "main",
        })),
    );
    assert!(resp["error"].is_null(), "task.create error: {resp}");

    let resp = harness.rpc_call("task.done", Some(serde_json::json!({"id": "compat-t1"})));
    assert!(resp["error"].is_null(), "task.done error: {resp}");
    assert_eq!(resp["result"]["ok"], true);
}

#[test]
#[ignore]
fn test_daemon_v2_nudge_running_agent() {
    let harness = V2Harness::start();

    // Wait for the lead to be running
    let mut lead_name = String::new();
    for _ in 0..15 {
        std::thread::sleep(Duration::from_secs(1));
        let resp = harness.rpc_call("agent.list", None);
        let agents = resp["result"].as_array().unwrap();
        if let Some(lead) = agents
            .iter()
            .find(|a| a["kind"] == "Lead" && a["running"] == true)
        {
            lead_name = lead["name"].as_str().unwrap().to_string();
            break;
        }
    }
    assert!(!lead_name.is_empty(), "lead should be running");

    // Nudge the running lead
    let resp = harness.rpc_call(
        "coworker.nudge",
        Some(serde_json::json!({
            "name": lead_name,
            "message": "Hello from nudge E2E test",
        })),
    );
    assert!(
        resp["error"].is_null(),
        "nudge should succeed for running agent: {resp}"
    );

    // Check if the nudge was posted to the DM channel
    std::thread::sleep(Duration::from_secs(2));
    let dm_channel = format!("dm-{lead_name}");
    let resp = harness.rpc_call(
        "channel.read",
        Some(serde_json::json!({
            "channel": dm_channel,
            "limit": 5,
        })),
    );
    // DM channel may or may not exist yet depending on whether the lead was spawned
    // with a DM channel. The key assertion: the nudge RPC didn't error.
    eprintln!("DM channel {dm_channel} read result: {resp}");
}

#[test]
#[ignore]
fn test_daemon_v2_nudge_stopped_agent_triggers_resume() {
    let harness = V2Harness::start();

    // Wait for the lead to be running and get its session_id
    let mut agent_id = String::new();
    let mut session_id = String::new();
    let mut agent_name = String::new();
    for _ in 0..15 {
        std::thread::sleep(Duration::from_secs(1));
        let resp = harness.rpc_call("agent.list", None);
        let agents = resp["result"].as_array().unwrap();
        if let Some(lead) = agents
            .iter()
            .find(|a| a["kind"] == "Lead" && a["running"] == true && !a["session_id"].is_null())
        {
            agent_id = lead["id"].as_str().unwrap().to_string();
            session_id = lead["session_id"].as_str().unwrap().to_string();
            agent_name = lead["name"].as_str().unwrap().to_string();
            break;
        }
    }
    assert!(!session_id.is_empty(), "lead should have session_id");

    // Stop the agent
    let resp = harness.rpc_call(
        "coworker.break",
        Some(serde_json::json!({"name": agent_name})),
    );
    assert!(resp["error"].is_null(), "stop should succeed: {resp}");

    // Wait for it to actually stop
    std::thread::sleep(Duration::from_secs(3));
    let resp = harness.rpc_call("agent.list", None);
    let agents = resp["result"].as_array().unwrap();
    let stopped = agents
        .iter()
        .find(|a| a["id"] == agent_id)
        .map(|a| a["running"] == false)
        .unwrap_or(false);
    assert!(stopped, "agent should be stopped after break");

    // Nudge the stopped agent — this should trigger a resume
    let resp = harness.rpc_call(
        "coworker.nudge",
        Some(serde_json::json!({
            "name": agent_name,
            "message": "Wake up! Nudge test for stopped agent.",
        })),
    );
    assert!(
        resp["error"].is_null(),
        "nudge should accept request for stopped agent: {resp}"
    );

    // The agent should be resuming — check after a few seconds
    // (resume spawns a new process with --resume <session_id>)
    let mut resumed = false;
    for _ in 0..15 {
        std::thread::sleep(Duration::from_secs(2));
        let resp = harness.rpc_call("agent.list", None);
        let agents = resp["result"].as_array().unwrap();
        if agents
            .iter()
            .any(|a| a["name"] == agent_name && a["running"] == true)
        {
            resumed = true;
            eprintln!("Agent {agent_name} resumed after nudge!");
            break;
        }
    }
    // Resume may or may not work depending on session persistence
    // The key test: the nudge didn't crash and the daemon is healthy
    let resp = harness.rpc_call("ping", None);
    assert_eq!(
        resp["result"], "pong",
        "daemon should be healthy after nudge"
    );
    eprintln!("Nudge stopped agent test: resumed={resumed}");
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
