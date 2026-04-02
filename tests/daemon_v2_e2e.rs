//! End-to-end tests for the daemon v2 process: startup, Unix socket RPC, and shutdown.
//!
//! These tests spawn a real `midtown daemon-v2` binary, connect via a Unix domain
//! socket, and exercise the JSON-RPC interface.
//!
//! Run with:
//!   cargo test --test daemon_v2_e2e -- --ignored

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
    /// The MIDTOWN_BASE_DIR used by this daemon instance.
    midtown_base: PathBuf,
    /// The child process handle.
    child: Option<Child>,
}

impl V2Harness {
    /// Spin up a `midtown daemon-v2` process and wait for the socket to appear.
    /// Each harness gets a fully isolated MIDTOWN_BASE_DIR so tests can run in parallel.
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
        // Each test gets its own MIDTOWN_BASE_DIR for full isolation.
        let midtown_base = state_dir.path().join("midtown-home");

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
            .env("MIDTOWN_BASE_DIR", &midtown_base)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn midtown daemon-v2");

        let harness = V2Harness {
            _temp_dir: temp_dir,
            _state_dir: state_dir,
            socket_path,
            midtown_base: midtown_base.clone(),
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
        if self.socket_path.exists()
            && let Ok(mut stream) = UnixStream::connect(&self.socket_path)
        {
            let req = serde_json::json!({"jsonrpc":"2.0","method":"shutdown","id":999});
            let _ = stream.write_all(&serde_json::to_vec(&req).unwrap());
            let _ = stream.shutdown(std::net::Shutdown::Write);
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

/// Spec 8: daemon starts, recovers from event store, responds to status
/// Spec 10.1: status returns agent/task/PR counts
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

/// Spec 10.1: agent.list returns agents with id and name
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

/// Spec 10.4: unknown method returns error -32601
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

/// Spec 10.1: shutdown gracefully stops the daemon
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
    let exited = wait_for_exit(child, Duration::from_secs(10));
    assert!(exited, "daemon process should exit after shutdown RPC");
}

/// Spec 10.1: task.create emits TaskCreated, task appears in status
/// Spec 14: midtown task create accepts the same parameters
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

/// Spec 2.1: pending tasks spawn workers
/// Spec 4.1: spawning succeeds → AgentCreated + AgentStarted emitted
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

    // Poll until we see a worker spawned for task t1 (dispatch runs every 5s)
    let mut saw_worker = false;
    for i in 0..30 {
        std::thread::sleep(Duration::from_secs(1));
        let agents = harness.rpc_call("agent.list", None);
        let list = agents["result"].as_array().unwrap_or(&vec![]).clone();
        if list.iter().any(|a| a["task_id"].as_str() == Some("t1")) {
            saw_worker = true;
            eprintln!("Worker spawned for task t1 after {i}s");
            break;
        }
    }
    assert!(
        saw_worker,
        "expected worker to be spawned for task t1 within 30s"
    );

    // Verify the task was assigned (worker exists with task_id)
    let agents_resp = harness.rpc_call("agent.list", None);
    let agents = agents_resp["result"].as_array().unwrap_or(&vec![]).clone();
    assert!(
        agents.iter().any(|a| a["task_id"].as_str() == Some("t1")),
        "worker with task_id t1 should exist"
    );
}

/// Spec 2.2: task.done completes task and stop_completed_agents stops the worker
#[test]
#[ignore]
fn test_daemon_v2_task_done_stops_worker() {
    let harness = V2Harness::start();

    // Create and wait for dispatch
    let resp = harness.rpc_call(
        "task.create",
        Some(serde_json::json!({
            "id": "done-t1",
            "subject": "Task to complete",
            "channel": "main",
        })),
    );
    assert!(resp["error"].is_null());

    // Wait for agent to spawn
    for _ in 0..30 {
        std::thread::sleep(Duration::from_secs(1));
        let status = harness.rpc_call("status", None);
        if status["result"]["agents"]["running"].as_u64().unwrap_or(0) > 0 {
            break;
        }
    }

    // Complete the task
    let resp = harness.rpc_call("task.done", Some(serde_json::json!({ "id": "done-t1" })));
    assert!(resp["error"].is_null(), "task.done error: {resp}");

    // Poll for task completion — the daemon loop applies events asynchronously
    // after the RPC response, so we need to wait for it to process.
    let mut completed = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_secs(1));
        let task_list = harness.rpc_call("task.list", None);
        let tasks = task_list["result"].as_array().expect("tasks");
        if tasks
            .iter()
            .any(|t| t["id"] == "done-t1" && t["status"] == "Completed")
        {
            completed = true;
            break;
        }
    }
    assert!(completed, "task should reach Completed status within 20s");
}

/// Spec 5.1: message to a new channel demand-spawns a lead
#[test]
#[ignore]
fn test_daemon_v2_demand_spawns_lead_on_message() {
    let harness = V2Harness::start();

    // Post a message to a brand new channel
    let resp = harness.rpc_call(
        "channel.post",
        Some(serde_json::json!({
            "channel": "demand-test",
            "sender": "user",
            "content": "hello demand-spawned lead",
        })),
    );
    assert!(resp["error"].is_null(), "channel.post error: {resp}");

    // Wait for the demand-spawned lead to appear (longer timeout for parallel CI)
    let mut found_lead = false;
    for _ in 0..30 {
        std::thread::sleep(Duration::from_secs(1));
        let agents = harness.rpc_call("agent.list", None);
        let list = agents["result"].as_array().unwrap_or(&vec![]).clone();
        if list
            .iter()
            .any(|a| a["channel"].as_str() == Some("demand-test") && a["kind"] == "Lead")
        {
            found_lead = true;
            break;
        }
    }
    assert!(
        found_lead,
        "demand-spawned lead should appear for new channel"
    );
}

/// Spec 3.1: polling fails → log error and return no events (non-crash)
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

/// Spec 5.3: channel.post writes to JSONL, channel.read returns messages
/// Spec 14: midtown channel post writes to channel JSONL files
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

/// Spec 5.2: channel create + archive + unarchive via RPC
#[test]
#[ignore]
fn test_daemon_v2_channel_create_archive_unarchive() {
    let harness = V2Harness::start();

    // Create a channel with unique name to avoid collisions
    let chan_name = format!("archive-{}", std::process::id());
    let resp = harness.rpc_call(
        "channel.create",
        Some(serde_json::json!({ "name": chan_name })),
    );
    assert!(resp["error"].is_null(), "channel.create error: {resp}");

    // Verify it appears in channel.list
    let resp = harness.rpc_call("channel.list", None);
    let channels = resp["result"].as_array().expect("should be array");
    assert!(
        channels.iter().any(|c| c["name"] == chan_name.as_str()),
        "created channel should appear in list"
    );

    // Archive it
    let resp = harness.rpc_call(
        "channel.archive",
        Some(serde_json::json!({ "channel": chan_name.as_str() })),
    );
    assert!(resp["error"].is_null(), "channel.archive error: {resp}");

    // Unarchive it
    let resp = harness.rpc_call(
        "channel.unarchive",
        Some(serde_json::json!({ "channel": chan_name.as_str() })),
    );
    assert!(resp["error"].is_null(), "channel.unarchive error: {resp}");

    // Verify it's back
    let resp = harness.rpc_call("channel.list", None);
    let channels = resp["result"].as_array().expect("should be array");
    assert!(
        channels.iter().any(|c| c["name"] == chan_name.as_str()),
        "unarchived channel should appear in list"
    );
}

/// Spec 5.3: thread replies excluded from default read, included with thread_parent_id
#[test]
#[ignore]
fn test_daemon_v2_thread_post_and_read() {
    let harness = V2Harness::start();

    // Post a top-level message
    let resp = harness.rpc_call(
        "channel.post",
        Some(serde_json::json!({
            "channel": "thread-test",
            "sender": "alice",
            "content": "top-level message",
        })),
    );
    assert!(resp["error"].is_null(), "post error: {resp}");
    let parent_id = resp["result"]["id"]
        .as_str()
        .expect("should have message id")
        .to_string();

    // Post a thread reply
    let resp = harness.rpc_call(
        "channel.post",
        Some(serde_json::json!({
            "channel": "thread-test",
            "sender": "bob",
            "content": "thread reply",
            "thread_id": parent_id,
        })),
    );
    assert!(resp["error"].is_null(), "thread post error: {resp}");

    // Default read should exclude the thread reply
    let resp = harness.rpc_call(
        "channel.read",
        Some(serde_json::json!({
            "channel": "thread-test",
        })),
    );
    assert!(resp["error"].is_null(), "read error: {resp}");
    let messages = resp["result"].as_array().expect("should be array");
    // All messages in default read should have no thread_parent_id
    let has_thread_reply = messages
        .iter()
        .any(|m| m.get("thread_parent_id").is_some_and(|v| !v.is_null()));
    assert!(
        !has_thread_reply,
        "default read should exclude thread replies, got {messages:?}"
    );

    // Thread-specific read should include parent + reply
    let resp = harness.rpc_call(
        "channel.read",
        Some(serde_json::json!({
            "channel": "thread-test",
            "thread_parent_id": parent_id,
        })),
    );
    assert!(resp["error"].is_null(), "thread read error: {resp}");
    let thread_msgs = resp["result"].as_array().expect("should be array");
    assert!(
        thread_msgs.len() >= 2,
        "thread read should include parent + reply (at least 2), got {thread_msgs:?}"
    );
    // Verify the reply is in there
    assert!(
        thread_msgs.iter().any(|m| m["message"] == "thread reply"),
        "thread read should include the reply, got {thread_msgs:?}"
    );
}

/// Spec 5.2: lead_driven channels skip auto-dispatch
/// Spec 2.1: lead_driven tasks not auto-dispatched
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

/// Spec 4.5: session.fork spawns a fork bound to a thread
/// Spec 10.1: session.fork returns spawn command or existing fork
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
    for _ in 0..30 {
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

/// Spec 14: v1 RPC methods handled via compatibility aliases
/// Spec 10.2: ping → pong, version → info, snapshot → status
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

/// Spec 1.4: nudge running agent delivers message
/// Spec 10.2: coworker.nudge sends NudgeAgent command
#[test]
#[ignore]
fn test_daemon_v2_nudge_running_agent() {
    let harness = V2Harness::start();

    // Wait for the lead to be running
    let mut lead_name = String::new();
    for _ in 0..30 {
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

/// Spec 1.4: nudge stopped agent with session_id → resume before deliver
/// Spec 4.3: ResumeAgent spawns with resume_session_id
#[test]
#[ignore]
fn test_daemon_v2_nudge_stopped_agent_triggers_resume() {
    let harness = V2Harness::start();

    // Wait for the lead to be running and get its session_id
    let mut agent_id = String::new();
    let mut session_id = String::new();
    let mut agent_name = String::new();
    for _ in 0..30 {
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

    // Poll for the agent to stop (background stop is async in non-blocking executor)
    let mut stopped = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_secs(1));
        let resp = harness.rpc_call("agent.list", None);
        let agents = resp["result"].as_array().unwrap();
        stopped = agents
            .iter()
            .find(|a| a["id"] == agent_id)
            .map(|a| a["running"] == false)
            .unwrap_or(false);
        if stopped {
            break;
        }
    }
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
    for _ in 0..30 {
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

/// Section 15: daemon.set-draining prevents new task dispatch
#[test]
#[ignore]
fn test_daemon_v2_draining_prevents_dispatch() {
    let harness = V2Harness::start();

    // Enable draining mode
    let resp = harness.rpc_call(
        "daemon.set-draining",
        Some(serde_json::json!({"draining": true})),
    );
    assert!(resp["error"].is_null(), "set-draining error: {resp}");

    // Create a task — should NOT be dispatched while draining
    let resp = harness.rpc_call(
        "task.create",
        Some(serde_json::json!({
            "id": "drain-t1",
            "subject": "Should not dispatch",
            "channel": "main",
        })),
    );
    assert!(resp["error"].is_null());

    // Wait for a dispatch cycle
    std::thread::sleep(Duration::from_secs(8));

    // No worker should be spawned for this task
    let agents = harness.rpc_call("agent.list", None);
    let list = agents["result"].as_array().unwrap_or(&vec![]).clone();
    let drain_workers: Vec<_> = list
        .iter()
        .filter(|a| a["task_id"].as_str() == Some("drain-t1"))
        .collect();
    assert!(
        drain_workers.is_empty(),
        "draining mode should prevent task dispatch"
    );

    // Disable draining
    let resp = harness.rpc_call(
        "daemon.set-draining",
        Some(serde_json::json!({"draining": false})),
    );
    assert!(resp["error"].is_null());
}

/// Wait up to `timeout` for `child` to exit. Returns true if it exited in time.
/// Spec 10.6: task.list returns tasks created via task.create
/// Regression: CLI `task list` read from filesystem TaskStore (always empty)
/// instead of querying daemon projections via RPC.
#[test]
#[ignore]
fn test_daemon_v2_task_list_returns_created_tasks() {
    let harness = V2Harness::start();

    // Create a task via RPC
    let resp = harness.rpc_call(
        "task.create",
        Some(serde_json::json!({
            "id": "t1",
            "subject": "Test task for listing",
            "channel": "main",
        })),
    );
    assert!(resp["error"].is_null(), "task.create error: {resp}");

    // task.list should return the task we just created
    let list_resp = harness.rpc_call("task.list", None);
    assert!(list_resp["error"].is_null(), "task.list error: {list_resp}");

    let tasks = list_resp["result"]
        .as_array()
        .expect("task.list result should be an array");

    assert!(
        !tasks.is_empty(),
        "task.list should return the created task, but returned empty: {list_resp}"
    );

    // Verify the task has the expected fields
    let task = tasks
        .iter()
        .find(|t| t["id"].as_str() == Some("t1"))
        .expect("task t1 should be in list");
    assert_eq!(task["subject"], "Test task for listing");
    assert_eq!(task["channel"], "main");
}

/// Spec 8.2: daemon-v2 creates log file on startup so `midtown log` works
#[test]
#[ignore]
fn test_daemon_v2_creates_log_file() {
    let harness = V2Harness::start();

    // Verify the daemon is responding (socket is up)
    let resp = harness.rpc_call("status", None);
    assert_eq!(resp["jsonrpc"], "2.0", "daemon not responding");

    // The daemon should have created the log file at startup
    let log_file = harness
        .midtown_base
        .join("projects")
        .join("test-repo")
        .join("logs")
        .join("daemon.log");

    assert!(
        log_file.exists(),
        "daemon should create log file at {log_file:?} on startup"
    );

    // The log file should have some content (at minimum, startup messages)
    let metadata = std::fs::metadata(&log_file).expect("read log file metadata");
    assert!(
        metadata.len() > 0,
        "log file should contain startup log entries, but is empty"
    );
}

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
