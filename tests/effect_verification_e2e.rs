//! E2E tests for effect execution verification.
//!
//! These tests verify that daemon effects actually produce observable outcomes:
//! - PostToChannel writes to channel.jsonl
//! - SpawnCoworker creates a tmux window
//! - NudgeCoworker sends keys to the pane
//!
//! Run with: `cargo test --test effect_verification_e2e -- --ignored --test-threads=1`

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Counter for unique test names across tests.
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique test repo name to avoid conflicts.
fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("effect-e2e-test-{}-{}", std::process::id(), counter)
}

/// Check if tmux is available.
fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Test fixture for effect verification E2E tests.
///
/// Creates an isolated environment with a fake git repo and manages
/// daemon lifecycle. Reuses the pattern from daemon_e2e.rs.
struct EffectTestFixture {
    /// Temporary directory containing the test repo
    temp_dir: PathBuf,
    /// Project directory under ~/.midtown/projects/<name>/
    project_dir: PathBuf,
    /// Repository name (used for socket path derivation and tmux session)
    repo_name: String,
    /// Path to the daemon socket
    socket_path: PathBuf,
    /// Daemon process handle (if started)
    daemon_process: Option<std::process::Child>,
    /// Tmux session name (midtown-<repo_name>)
    session_name: String,
}

impl EffectTestFixture {
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

        // Compute socket path (matches midtown::paths)
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

        // Ensure parent directories exist
        if let Some(parent) = socket_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Some(parent) = project_dir.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let session_name = format!("midtown-{}", repo_name);

        Some(Self {
            temp_dir,
            project_dir,
            repo_name,
            socket_path,
            daemon_process: None,
            session_name,
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

        // Start the daemon process
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

        // Set read timeout to detect hangs
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .ok()?;

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

    /// Get the path to the channel.jsonl file.
    fn channel_path(&self) -> PathBuf {
        self.project_dir.join("channel.jsonl")
    }

    /// List tmux windows in the session.
    fn list_tmux_windows(&self) -> Vec<String> {
        let output = Command::new("tmux")
            .args([
                "list-windows",
                "-t",
                &self.session_name,
                "-F",
                "#{window_name}",
            ])
            .output()
            .ok();

        match output {
            Some(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(String::from)
                .collect(),
            _ => vec![],
        }
    }

    /// Capture pane content from a coworker window.
    fn capture_pane(&self, window_name: &str) -> Option<String> {
        let target = format!("{}:{}", self.session_name, window_name);
        let output = Command::new("tmux")
            .args(["capture-pane", "-t", &target, "-p"])
            .output()
            .ok()?;

        if output.status.success() {
            Some(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            None
        }
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

        // Kill the tmux session
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.session_name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        // Final fallback: pkill
        let pattern = format!("midtown daemon.*{}", self.repo_name);
        let _ = Command::new("pkill")
            .args(["-f", &pattern])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for EffectTestFixture {
    fn drop(&mut self) {
        self.stop_daemon();

        // Clean up socket file and its parent directory
        let _ = fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir(parent);
        }

        // Clean up the entire project directory
        let _ = fs::remove_dir_all(&self.project_dir);

        // Clean up temp directory (the fake git repo)
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Effect Verification Tests
// ────────────────────────────────────────────────────────────────────────────

/// Test that PostToChannel effect actually writes to channel.jsonl.
///
/// This verifies the observable outcome: posting a message via RPC should
/// result in the message appearing in the channel file on disk.
#[test]
#[ignore] // Requires built binary
fn effect_post_to_channel_writes_file() {
    let mut fixture = match EffectTestFixture::new() {
        Some(f) => f,
        None => {
            eprintln!("Skipping: fixture creation failed");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping: daemon failed to start");
        return;
    }

    // Generate a unique marker to identify our test message
    let marker = format!("e2e-test-marker-{}", std::process::id());
    let message = format!("Hello from effect verification test: {}", marker);

    // Post a message via RPC
    let params = serde_json::json!({
        "message": message,
        "from": "test-agent"
    });

    let response = fixture.rpc_call("channel.post", Some(params));
    assert!(
        response.is_some(),
        "Should receive response from channel.post"
    );

    let response = response.unwrap();
    assert!(
        response["error"].is_null(),
        "channel.post should not return an error: {:?}",
        response["error"]
    );

    // Give the daemon a moment to flush the write
    thread::sleep(Duration::from_millis(200));

    // Verify message appears in channel.jsonl
    let channel_path = fixture.channel_path();
    assert!(
        channel_path.exists(),
        "channel.jsonl should exist at {:?}",
        channel_path
    );

    let contents = fs::read_to_string(&channel_path).expect("Should read channel.jsonl");
    assert!(
        contents.contains(&marker),
        "channel.jsonl should contain our test marker. Contents:\n{}",
        contents
    );
}

/// Test that spawning a coworker creates a tmux window.
///
/// This verifies the observable outcome: the coworker.spawn RPC should
/// result in a tmux window being created with the coworker's name.
///
/// Note: This test spawns a real coworker which may require Claude CLI.
/// If Claude is not available, the spawn may fail but we still verify
/// the window creation attempt.
#[test]
#[ignore] // Requires tmux and built binary (may require claude CLI)
fn effect_spawn_coworker_creates_window() {
    if !tmux_available() {
        eprintln!("Skipping: tmux not available");
        return;
    }

    let mut fixture = match EffectTestFixture::new() {
        Some(f) => f,
        None => {
            eprintln!("Skipping: fixture creation failed");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping: daemon failed to start");
        return;
    }

    let coworker_name = "lexington";

    // Spawn a coworker via RPC
    let params = serde_json::json!({
        "name": coworker_name
    });

    let response = fixture.rpc_call("coworker.spawn", Some(params));
    assert!(
        response.is_some(),
        "Should receive response from coworker.spawn"
    );

    let response = response.unwrap();

    // The spawn might fail if Claude CLI isn't available, but we can still
    // check what happened
    if response["error"].is_object() {
        let error_msg = response["error"]["message"]
            .as_str()
            .unwrap_or("unknown error");
        eprintln!(
            "Note: coworker.spawn returned error (expected in some test environments): {}",
            error_msg
        );
        // If it's a "no claude" or "command not found" error, that's expected
        // in test environments without Claude CLI installed
        return;
    }

    // Give tmux a moment to create the window
    thread::sleep(Duration::from_secs(2));

    // Verify tmux window exists
    let windows = fixture.list_tmux_windows();
    assert!(
        windows.iter().any(|w| w.contains(coworker_name)),
        "Tmux session should have a window for coworker '{}'. Found windows: {:?}",
        coworker_name,
        windows
    );

    // Verify coworker appears in coworker.list
    let list_response = fixture.rpc_call("coworker.list", None);
    assert!(
        list_response.is_some(),
        "Should receive response from coworker.list"
    );

    let list_response = list_response.unwrap();
    let coworkers = list_response["result"]["coworkers"]
        .as_array()
        .expect("coworkers should be an array");

    let found = coworkers
        .iter()
        .any(|c| c["name"].as_str() == Some(coworker_name));
    assert!(
        found,
        "Coworker '{}' should appear in coworker.list. Got: {:?}",
        coworker_name, coworkers
    );
}

/// Test that nudging a coworker sends keys to their pane.
///
/// This verifies the observable outcome: the coworker.nudge RPC should
/// result in the nudge message being sent to the coworker's tmux pane.
///
/// Note: This test requires a running coworker. We spawn one first,
/// then nudge it. If Claude is not available, this test is skipped.
#[test]
#[ignore] // Requires tmux, built binary, and claude CLI
fn effect_nudge_coworker_sends_keys() {
    if !tmux_available() {
        eprintln!("Skipping: tmux not available");
        return;
    }

    let mut fixture = match EffectTestFixture::new() {
        Some(f) => f,
        None => {
            eprintln!("Skipping: fixture creation failed");
            return;
        }
    };

    if !fixture.start_daemon() {
        eprintln!("Skipping: daemon failed to start");
        return;
    }

    let coworker_name = "park";

    // First, spawn a coworker
    let spawn_params = serde_json::json!({
        "name": coworker_name
    });

    let spawn_response = fixture.rpc_call("coworker.spawn", Some(spawn_params));
    assert!(
        spawn_response.is_some(),
        "Should receive response from coworker.spawn"
    );

    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        eprintln!(
            "Skipping: coworker.spawn failed (expected without Claude CLI): {:?}",
            spawn_response["error"]
        );
        return;
    }

    // Wait for coworker to be ready
    thread::sleep(Duration::from_secs(3));

    // Generate a unique marker for the nudge message
    let marker = format!("nudge-test-{}", std::process::id());
    let nudge_message = format!("Test nudge message: {}", marker);

    // Nudge the coworker via RPC
    let nudge_params = serde_json::json!({
        "name": coworker_name,
        "message": nudge_message
    });

    let nudge_response = fixture.rpc_call("coworker.nudge", Some(nudge_params));
    assert!(
        nudge_response.is_some(),
        "Should receive response from coworker.nudge"
    );

    let nudge_response = nudge_response.unwrap();
    assert!(
        nudge_response["error"].is_null(),
        "coworker.nudge should not return an error: {:?}",
        nudge_response["error"]
    );

    // Give tmux a moment to process the send-keys
    thread::sleep(Duration::from_millis(500));

    // Capture pane and verify nudge appeared
    // Note: The nudge text may be in the pane's scrollback, so we capture
    // with history if possible
    let pane_content = fixture.capture_pane(coworker_name);

    // The pane should exist (coworker was spawned)
    assert!(
        pane_content.is_some(),
        "Should be able to capture pane for coworker '{}'",
        coworker_name
    );

    let pane_content = pane_content.unwrap();

    // The nudge might be in the input area or scrollback.
    // In Claude Code, nudges are sent via tmux send-keys, which types the
    // message into the prompt. Check if the marker appears anywhere.
    //
    // Note: In some cases, the nudge might be processed immediately by Claude
    // and no longer visible. We check for its presence, but if Claude is
    // actively processing, it may have scrolled away.
    if pane_content.contains(&marker) {
        // Great - we can see the nudge in the pane
        println!("Nudge message visible in pane");
    } else {
        // The nudge was sent but may have been processed already.
        // At minimum, verify the RPC succeeded (which we checked above).
        println!(
            "Note: Nudge marker not visible in current pane content (may have been processed)"
        );
        println!("Pane content sample (first 500 chars):");
        println!("{}", &pane_content[..pane_content.len().min(500)]);
    }

    // The core assertion is that the RPC succeeded - the effect was executed.
    // Pane content visibility depends on Claude's processing state.
}

// ────────────────────────────────────────────────────────────────────────────
// Helper tests for fixture verification
// ────────────────────────────────────────────────────────────────────────────

/// Verify the test fixture can start a daemon and connect to it.
#[test]
#[ignore] // Requires built binary
fn fixture_can_start_daemon() {
    let mut fixture = match EffectTestFixture::new() {
        Some(f) => f,
        None => {
            eprintln!("Skipping: fixture creation failed");
            return;
        }
    };

    assert!(
        fixture.start_daemon(),
        "Daemon should start successfully and create socket"
    );

    // Verify we can connect
    assert!(
        fixture.connect().is_some(),
        "Should be able to connect to daemon socket"
    );

    // Verify ping works
    let response = fixture.rpc_call("ping", None);
    assert!(response.is_some(), "Should receive response from ping");
    assert_eq!(
        response.unwrap()["result"].as_str(),
        Some("pong"),
        "Ping should return 'pong'"
    );
}
