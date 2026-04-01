//! End-to-end tests for Codex agent support in the daemon v2.
//!
//! These tests spawn a real `midtown daemon-v2` with `fake-codex-cli` on the PATH
//! (symlinked as `codex`), then exercise the Codex-specific agent lifecycle via
//! the JSON-RPC interface.
//!
//! Run with:
//!   cargo test --test codex_v2_e2e -- --ignored --test-threads=1

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Locate the `fake-codex-cli` binary in the same target directory as `midtown`.
fn fake_codex_cli_path() -> PathBuf {
    let midtown = PathBuf::from(env!("CARGO_BIN_EXE_midtown"));
    let bin_dir = midtown
        .parent()
        .expect("midtown binary should have a parent dir");
    let path = bin_dir.join("fake-codex-cli");
    assert!(
        path.exists(),
        "fake-codex-cli not found at {:?} — run `cargo build` for the full workspace first",
        path
    );
    path
}

// ── Harness ───────────────────────────────────────────────────────────────────

/// Test harness that owns a running `midtown daemon-v2` with fake-codex-cli on PATH.
struct CodexV2Harness {
    _temp_dir: tempfile::TempDir,
    _state_dir: tempfile::TempDir,
    _bin_dir: tempfile::TempDir,
    _codex_home: tempfile::TempDir,
    socket_path: PathBuf,
    child: Option<Child>,
}

impl CodexV2Harness {
    fn start() -> Self {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let state_dir = tempfile::TempDir::new().expect("state dir");
        let bin_dir = tempfile::TempDir::new().expect("bin dir");
        let codex_home = tempfile::TempDir::new().expect("codex home");

        // Symlink fake-codex-cli as "codex" so the daemon finds it on PATH.
        std::os::unix::fs::symlink(fake_codex_cli_path(), bin_dir.path().join("codex"))
            .expect("symlink fake-codex-cli as codex");

        let path = format!(
            "{}:{}",
            bin_dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );

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
                "codex-test-repo",
                "--channel",
                "main",
            ])
            .current_dir(repo)
            .env("PATH", &path)
            .env("CODEX_HOME", codex_home.path())
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .env("MIDTOWN_WEBHOOK_PORT", "0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn midtown daemon-v2");

        let harness = CodexV2Harness {
            _temp_dir: temp_dir,
            _state_dir: state_dir,
            _bin_dir: bin_dir,
            _codex_home: codex_home,
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
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("shutdown write");

        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).expect("read response");

        serde_json::from_slice(&buf).expect("parse JSON response")
    }

    fn stop(&mut self) {
        if self.socket_path.exists() {
            let _ = std::panic::catch_unwind(|| {
                self.rpc_call("shutdown", None);
            });
        }

        if let Some(mut child) = self.child.take() {
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

impl Drop for CodexV2Harness {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Spawn a Codex agent via coworker.spawn with provider=codex, verify it appears
/// in agent.list with the correct provider field.
#[test]
#[ignore]
fn test_codex_spawn_via_rpc() {
    let harness = CodexV2Harness::start();

    let resp = harness.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "name": "codex-worker-1",
            "channel": "main",
            "prompt": "say hello",
            "provider": "codex",
        })),
    );
    assert!(resp["error"].is_null(), "coworker.spawn error: {resp}");
    assert_eq!(resp["result"]["ok"], true);

    // Poll for the agent to appear in agent.list
    let mut found = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_secs(1));
        let agents_resp = harness.rpc_call("agent.list", None);
        let agents = agents_resp["result"].as_array().unwrap_or(&vec![]).clone();
        if let Some(agent) = agents
            .iter()
            .find(|a| a["name"].as_str() == Some("codex-worker-1"))
        {
            assert_eq!(
                agent["provider"], "Codex",
                "agent should have Codex provider: {agent}"
            );
            assert_eq!(agent["kind"], "Worker");
            found = true;
            break;
        }
    }
    assert!(found, "Codex agent should appear in agent.list");
}

/// Spawn a Codex agent and verify it's counted in the status response.
#[test]
#[ignore]
fn test_codex_agent_shows_in_status() {
    let harness = CodexV2Harness::start();

    // Get baseline agent count
    let baseline = harness.rpc_call("status", None);
    let baseline_total = baseline["result"]["agents"]["total"].as_u64().unwrap_or(0);

    let resp = harness.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "name": "codex-status-1",
            "channel": "main",
            "prompt": "status test",
            "provider": "codex",
        })),
    );
    assert!(resp["error"].is_null(), "spawn error: {resp}");

    // Wait for agent to appear
    let mut count_increased = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_secs(1));
        let status = harness.rpc_call("status", None);
        let total = status["result"]["agents"]["total"].as_u64().unwrap_or(0);
        if total > baseline_total {
            count_increased = true;
            break;
        }
    }
    assert!(
        count_increased,
        "agent count should increase after spawning Codex agent"
    );
}

/// Spawn a Codex agent, then stop it with coworker.break.
#[test]
#[ignore]
fn test_codex_agent_stop() {
    let harness = CodexV2Harness::start();

    let resp = harness.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "name": "codex-stop-1",
            "channel": "main",
            "prompt": "stop test",
            "provider": "codex",
        })),
    );
    assert!(resp["error"].is_null(), "spawn error: {resp}");

    // Wait for the agent to be running
    let mut is_running = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_secs(1));
        let agents_resp = harness.rpc_call("agent.list", None);
        let agents = agents_resp["result"].as_array().unwrap_or(&vec![]).clone();
        if agents
            .iter()
            .any(|a| a["name"].as_str() == Some("codex-stop-1") && a["running"] == true)
        {
            is_running = true;
            break;
        }
    }
    assert!(is_running, "Codex agent should be running before stop");

    // Stop the agent
    let resp = harness.rpc_call(
        "coworker.break",
        Some(serde_json::json!({"name": "codex-stop-1"})),
    );
    assert!(resp["error"].is_null(), "stop error: {resp}");

    // Verify it transitions to not running
    let mut stopped = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_secs(1));
        let agents_resp = harness.rpc_call("agent.list", None);
        let agents = agents_resp["result"].as_array().unwrap_or(&vec![]).clone();
        if let Some(agent) = agents
            .iter()
            .find(|a| a["name"].as_str() == Some("codex-stop-1"))
            && agent["running"] == false
        {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "Codex agent should stop after coworker.break");
}

/// Spawn a Codex agent and nudge it — verify the nudge RPC succeeds.
#[test]
#[ignore]
fn test_codex_agent_nudge() {
    let harness = CodexV2Harness::start();

    let resp = harness.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "name": "codex-nudge-1",
            "channel": "main",
            "prompt": "nudge test",
            "provider": "codex",
        })),
    );
    assert!(resp["error"].is_null(), "spawn error: {resp}");

    // Wait for the agent to be running
    let mut is_running = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_secs(1));
        let agents_resp = harness.rpc_call("agent.list", None);
        let agents = agents_resp["result"].as_array().unwrap_or(&vec![]).clone();
        if agents
            .iter()
            .any(|a| a["name"].as_str() == Some("codex-nudge-1") && a["running"] == true)
        {
            is_running = true;
            break;
        }
    }
    assert!(is_running, "Codex agent should be running before nudge");

    // Nudge the agent
    let resp = harness.rpc_call(
        "coworker.nudge",
        Some(serde_json::json!({
            "name": "codex-nudge-1",
            "message": "Hello from Codex nudge E2E test",
        })),
    );
    assert!(
        resp["error"].is_null(),
        "nudge should succeed for running Codex agent: {resp}"
    );

    // Verify daemon is still healthy
    let resp = harness.rpc_call("ping", None);
    assert_eq!(
        resp["result"], "pong",
        "daemon should be healthy after Codex nudge"
    );
}

/// Spawn both a Claude (default) and Codex agent, verify mixed-platform coexistence.
#[test]
#[ignore]
fn test_mixed_platform_agents() {
    let harness = CodexV2Harness::start();

    // Spawn a Claude worker (default provider)
    let resp = harness.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "name": "claude-mixed-1",
            "channel": "main",
            "prompt": "claude worker",
        })),
    );
    assert!(resp["error"].is_null(), "claude spawn error: {resp}");

    // Spawn a Codex worker
    let resp = harness.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "name": "codex-mixed-1",
            "channel": "main",
            "prompt": "codex worker",
            "provider": "codex",
        })),
    );
    assert!(resp["error"].is_null(), "codex spawn error: {resp}");

    // Wait for both agents to appear
    let mut found_claude = false;
    let mut found_codex = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_secs(1));
        let agents_resp = harness.rpc_call("agent.list", None);
        let agents = agents_resp["result"].as_array().unwrap_or(&vec![]).clone();

        if !found_claude {
            found_claude = agents
                .iter()
                .any(|a| a["name"].as_str() == Some("claude-mixed-1"));
        }
        if !found_codex {
            found_codex = agents
                .iter()
                .any(|a| a["name"].as_str() == Some("codex-mixed-1"));
        }
        if found_claude && found_codex {
            break;
        }
    }
    assert!(found_claude, "Claude agent should appear in agent.list");
    assert!(found_codex, "Codex agent should appear in agent.list");

    // Verify providers are distinct
    let agents_resp = harness.rpc_call("agent.list", None);
    let agents = agents_resp["result"].as_array().unwrap();
    let claude_agent = agents
        .iter()
        .find(|a| a["name"].as_str() == Some("claude-mixed-1"))
        .expect("claude agent");
    let codex_agent = agents
        .iter()
        .find(|a| a["name"].as_str() == Some("codex-mixed-1"))
        .expect("codex agent");

    assert_eq!(claude_agent["provider"], "ClaudeCode");
    assert_eq!(codex_agent["provider"], "Codex");

    // Daemon should be healthy with mixed providers
    let resp = harness.rpc_call("status", None);
    assert!(resp["error"].is_null(), "status error with mixed agents");
    let total = resp["result"]["agents"]["total"].as_u64().unwrap_or(0);
    assert!(total >= 2, "should have at least 2 agents (mixed platform)");
}
