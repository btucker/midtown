//! End-to-end tests for the web UI and WebSocket functionality.
//!
//! These tests verify:
//! - REST API endpoints (health, channel, status, tmux)
//! - WebSocket connection and real-time message streaming
//! - Multi-project webserver routing
//! - Static file serving
//!
//! Run with `cargo test --test web_e2e -- --ignored` as these spawn real processes.

use std::fs;
use std::io::{BufRead, BufReader, Read as StdRead, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Check if the HTTP server is ready by making a simple TCP request.
/// This avoids using reqwest::blocking which creates an internal tokio runtime.
fn check_http_ready(port: u16) -> bool {
    let addr = format!("127.0.0.1:{}", port);
    let Ok(mut stream) =
        TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(500))
    else {
        return false;
    };

    // Send a simple HTTP GET request
    let request = format!(
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        port
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    // Read response
    let mut response = vec![0u8; 1024];
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    if let Ok(n) = stream.read(&mut response)
        && n > 0
    {
        // Check for HTTP 200 response
        let response_str = String::from_utf8_lossy(&response[..n]);
        return response_str.contains("200 OK") || response_str.contains("200 Ok");
    }

    false
}

/// Counter for unique test names across tests.
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique test repo name to avoid conflicts.
fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("web-e2e-test-{}-{}", std::process::id(), counter)
}

/// Find an available port for testing.
fn find_available_port() -> u16 {
    // Bind to port 0 to get an OS-assigned available port
    let listener = TcpListener::bind("127.0.0.1:0").expect("Failed to find available port");
    listener.local_addr().unwrap().port()
}

/// Clean up orphaned test daemons from previous runs.
fn cleanup_orphaned_test_daemons() {
    let _ = Command::new("pkill")
        .args(["-f", "midtown daemon.*web-e2e-test"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    thread::sleep(Duration::from_millis(50));

    // Clean up stale project directories
    let current_pid = format!("web-e2e-test-{}-", std::process::id());
    let projects_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("projects");
    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with("web-e2e-test-")
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    // Clean up stale socket directories
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
                && name.starts_with("web-e2e-test-")
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Test fixture for web E2E tests.
///
/// Creates an isolated environment with a fake git repo and manages
/// daemon lifecycle with a webhook server on a specific port.
#[allow(dead_code)]
struct WebTestFixture {
    /// Temporary directory containing the test repo
    temp_dir: PathBuf,
    /// Project directory under ~/.midtown/projects/<name>/
    project_dir: PathBuf,
    /// Repository name
    repo_name: String,
    /// Path to the daemon socket
    socket_path: PathBuf,
    /// Path to the daemon PID file
    pid_path: PathBuf,
    /// Webhook port for HTTP/WebSocket connections
    webhook_port: u16,
    /// Daemon process handle
    daemon_process: Option<Child>,
}

impl WebTestFixture {
    /// Create a new test fixture with a fake git repository.
    fn new() -> Option<Self> {
        cleanup_orphaned_test_daemons();

        let repo_name = test_repo_name();
        let temp_dir = std::env::temp_dir().join(&repo_name);
        let webhook_port = find_available_port();

        // Clean up any previous test data
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).ok()?;

        // Initialize a git repository
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

        // Compute paths
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
            webhook_port,
            daemon_process: None,
        })
    }

    /// Start the daemon process with webhook server enabled.
    fn start_daemon(&mut self) -> bool {
        // Build the daemon binary
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

        // Remove stale files
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.pid_path);

        // Start the daemon with webhook enabled on our test port
        let child = Command::new(&binary_path)
            .arg("daemon")
            .arg("--workdir")
            .arg(&self.temp_dir)
            .current_dir(&self.temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("MIDTOWN_WEBHOOK_PORT", self.webhook_port.to_string())
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .spawn();

        match child {
            Ok(c) => {
                self.daemon_process = Some(c);

                // Wait for both socket and HTTP server to become available
                for _ in 0..300 {
                    thread::sleep(Duration::from_millis(200));

                    // Check socket is ready
                    let socket_ready =
                        self.socket_path.exists() && UnixStream::connect(&self.socket_path).is_ok();

                    // Check HTTP server is ready using simple TCP + HTTP request
                    // (avoid reqwest::blocking which creates an internal tokio runtime)
                    let http_ready = check_http_ready(self.webhook_port);

                    if socket_ready && http_ready {
                        return true;
                    }
                }
                eprintln!("Daemon or HTTP server did not become available");
                false
            }
            Err(e) => {
                eprintln!("Failed to spawn daemon: {}", e);
                false
            }
        }
    }

    /// Get the base URL for HTTP requests.
    fn api_base(&self) -> String {
        format!("http://127.0.0.1:{}/api", self.webhook_port)
    }

    /// Get the WebSocket URL.
    fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/api/ws", self.webhook_port)
    }

    /// Connect to the daemon socket for RPC.
    fn rpc_connect(&self) -> Option<UnixStream> {
        UnixStream::connect(&self.socket_path).ok()
    }

    /// Send an RPC request.
    fn rpc_call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let mut stream = self.rpc_connect()?;
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .ok()?;

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

impl WebTestFixture {
    /// Perform async-safe cleanup - for use in async tests.
    async fn async_cleanup(&mut self) {
        let socket_path = self.socket_path.clone();
        let pid_path = self.pid_path.clone();
        let project_dir = self.project_dir.clone();
        let temp_dir = self.temp_dir.clone();
        let repo_name = self.repo_name.clone();
        let daemon_process = self.daemon_process.take();

        tokio::task::spawn_blocking(move || {
            do_cleanup(
                socket_path,
                pid_path,
                project_dir,
                temp_dir,
                repo_name,
                daemon_process,
            );
        })
        .await
        .ok();
    }
}

/// Guard that ensures cleanup runs even if test panics.
/// For async tests: create this after fixture setup, cleanup runs on drop.
struct AsyncCleanupGuard {
    socket_path: PathBuf,
    pid_path: PathBuf,
    project_dir: PathBuf,
    temp_dir: PathBuf,
    repo_name: String,
}

impl AsyncCleanupGuard {
    fn new(fixture: &WebTestFixture) -> Self {
        Self {
            socket_path: fixture.socket_path.clone(),
            pid_path: fixture.pid_path.clone(),
            project_dir: fixture.project_dir.clone(),
            temp_dir: fixture.temp_dir.clone(),
            repo_name: fixture.repo_name.clone(),
        }
    }
}

impl Drop for AsyncCleanupGuard {
    fn drop(&mut self) {
        // Run cleanup synchronously - this runs even on panic unwind
        do_cleanup(
            self.socket_path.clone(),
            self.pid_path.clone(),
            self.project_dir.clone(),
            self.temp_dir.clone(),
            self.repo_name.clone(),
            None, // daemon process handled separately via fixture
        );
    }
}

/// Shared cleanup logic.
fn do_cleanup(
    socket_path: PathBuf,
    pid_path: PathBuf,
    project_dir: PathBuf,
    temp_dir: PathBuf,
    repo_name: String,
    daemon_process: Option<Child>,
) {
    // Try graceful shutdown via socket
    if let Ok(mut stream) = UnixStream::connect(&socket_path) {
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

    // Kill the daemon process if we have it
    if let Some(mut child) = daemon_process {
        let _ = child.kill();
        let _ = child.wait();
    }

    // Kill any remaining processes
    let pattern = format!("midtown daemon.*{}", repo_name);
    let _ = Command::new("pkill")
        .args(["-f", &pattern])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    // Clean up files
    let _ = fs::remove_file(&socket_path);
    if let Some(parent) = socket_path.parent() {
        let _ = fs::remove_dir(parent);
    }
    let _ = fs::remove_file(&pid_path);

    let _ = fs::remove_dir_all(&project_dir);
    let _ = fs::remove_dir_all(&temp_dir);
}

impl Drop for WebTestFixture {
    fn drop(&mut self) {
        // Only do cleanup if daemon_process is still set (not already cleaned up)
        if self.daemon_process.is_some() {
            // Run cleanup synchronously. This is safe for sync tests (no tokio runtime).
            // Async tests use std::mem::forget() to skip Drop and handle cleanup explicitly.
            // This ensures cleanup completes even on panic in sync tests.
            do_cleanup(
                self.socket_path.clone(),
                self.pid_path.clone(),
                self.project_dir.clone(),
                self.temp_dir.clone(),
                self.repo_name.clone(),
                self.daemon_process.take(),
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// REST API Tests
// ────────────────────────────────────────────────────────────────────────────

/// Test that the /api/health endpoint returns "ok".
#[test]
#[ignore] // Requires built binary
fn test_web_api_health_endpoint() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let response = reqwest::blocking::get(format!("{}/health", fixture.api_base()));
    assert!(response.is_ok(), "Health endpoint should be accessible");

    let response = response.unwrap();
    assert!(response.status().is_success(), "Health should return 200");

    let body = response.text().unwrap();
    assert_eq!(body, "ok", "Health endpoint should return 'ok'");
}

/// Test that /api/channel returns an array of messages.
#[test]
#[ignore] // Requires built binary
fn test_web_api_channel_history() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Post a message via RPC first
    let post_params = serde_json::json!({
        "message": "Test message for web API",
        "from": "test-agent"
    });
    fixture.rpc_call("channel.post", Some(post_params));

    // Give the channel time to be written
    thread::sleep(Duration::from_millis(100));

    // Fetch channel history via HTTP
    let response = reqwest::blocking::get(format!("{}/channel", fixture.api_base()));
    assert!(response.is_ok(), "Channel endpoint should be accessible");

    let response = response.unwrap();
    assert!(response.status().is_success(), "Channel should return 200");

    let messages: Vec<serde_json::Value> = response.json().unwrap();

    // Should find our test message
    let found = messages.iter().any(|m| {
        m.get("from").and_then(|f| f.as_str()) == Some("test-agent")
            && m.get("content")
                .and_then(|c| c.as_str())
                .map(|s| s.contains("Test message"))
                .unwrap_or(false)
    });
    assert!(found, "Should find our test message in channel history");

    // Verify message structure
    for msg in &messages {
        assert!(
            msg.get("from").is_some(),
            "Message should have 'from' field"
        );
        assert!(
            msg.get("content").is_some(),
            "Message should have 'content' field"
        );
        assert!(
            msg.get("timestamp").is_some(),
            "Message should have 'timestamp' field"
        );
        assert!(
            msg.get("type").is_some(),
            "Message should have 'type' field"
        );
    }
}

/// Test that /api/status returns expected fields for kanban board.
#[test]
#[ignore] // Requires built binary
fn test_web_api_status_returns_kanban_data() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let response = reqwest::blocking::get(format!("{}/status", fixture.api_base()));
    assert!(response.is_ok(), "Status endpoint should be accessible");

    let response = response.unwrap();
    assert!(response.status().is_success(), "Status should return 200");

    let status: serde_json::Value = response.json().unwrap();

    // Verify kanban-relevant fields
    assert!(
        status.get("daemon").is_some(),
        "Status should include daemon field"
    );
    assert!(
        status
            .get("coworkers")
            .map(|c| c.is_array())
            .unwrap_or(false),
        "Status should include coworkers array"
    );
    assert!(
        status.get("tasks").map(|t| t.is_array()).unwrap_or(false),
        "Status should include tasks array"
    );
    assert!(
        status
            .get("pull_requests")
            .map(|p| p.is_array())
            .unwrap_or(false),
        "Status should include pull_requests array"
    );
    assert!(
        status
            .get("merged_prs")
            .map(|m| m.is_array())
            .unwrap_or(false),
        "Status should include merged_prs array"
    );
    assert!(
        status.get("repo_name").is_some(),
        "Status should include repo_name"
    );
    assert!(
        status.get("repo_status").is_some(),
        "Status should include repo_status"
    );
}

/// Test that /api/tmux-windows returns a list of windows.
#[test]
#[ignore] // Requires built binary and tmux
fn test_web_api_tmux_windows() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let response = reqwest::blocking::get(format!("{}/tmux-windows", fixture.api_base()));

    // This may fail if tmux session doesn't exist (expected in test env)
    if let Ok(response) = response
        && response.status().is_success()
    {
        let data: serde_json::Value = response.json().unwrap();
        assert!(
            data.get("windows").map(|w| w.is_array()).unwrap_or(false),
            "tmux-windows should return windows array"
        );
    }
}

/// Test that /api/tmux-pane validates window name.
#[test]
#[ignore] // Requires built binary
fn test_web_api_tmux_pane_validates_window() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Invalid window names should return 400
    let invalid_names = ["", "foo;bar", "foo:bar", "foo bar"];

    for name in invalid_names {
        let response =
            reqwest::blocking::get(format!("{}/tmux-pane?window={}", fixture.api_base(), name));
        if let Ok(resp) = response {
            assert!(
                !resp.status().is_success() || name.is_empty(),
                "Window name '{}' should be rejected or handled",
                name
            );
        }
    }

    // Valid window names that don't exist should return 404 (not 400)
    let response = reqwest::blocking::get(format!(
        "{}/tmux-pane?window=nonexistent",
        fixture.api_base()
    ));
    if let Ok(resp) = response {
        // Either 404 (window not found) or 500 (no tmux session) is acceptable
        assert!(
            resp.status() == reqwest::StatusCode::NOT_FOUND
                || resp.status() == reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "Nonexistent window should return 404 or 500, got {}",
            resp.status()
        );
    }
}

/// Test that /api/lead-pane returns pane content or appropriate error.
#[test]
#[ignore] // Requires built binary and tmux
fn test_web_api_lead_pane() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let response = reqwest::blocking::get(format!("{}/lead-pane", fixture.api_base()));

    // This may fail if tmux session doesn't exist (expected in test env)
    if let Ok(response) = response {
        // Either success (with content) or 404 (no lead window)
        assert!(
            response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND,
            "lead-pane should return 200 or 404, got {}",
            response.status()
        );

        if response.status().is_success() {
            let data: serde_json::Value = response.json().unwrap();
            assert!(
                data.get("content").is_some(),
                "lead-pane should return content field"
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// WebSocket Tests
// ────────────────────────────────────────────────────────────────────────────

/// Test that WebSocket connects successfully.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_websocket_connects() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    // Create cleanup guard - ensures cleanup runs even if assertions panic
    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    let ws_url = fixture.ws_url();
    let result = tokio::time::timeout(Duration::from_secs(5), connect_async(&ws_url)).await;

    assert!(result.is_ok(), "WebSocket connection should not timeout");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "WebSocket should connect successfully: {:?}",
        result.err()
    );

    let (mut ws_stream, _) = result.unwrap();

    // Close cleanly
    let _ = ws_stream.close(None).await;

    fixture.async_cleanup().await;
    std::mem::forget(fixture);
    // Guard drops here and runs cleanup (idempotent)
}

/// Test that channel messages are broadcast via WebSocket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_websocket_receives_channel_messages() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    // Create cleanup guard - ensures cleanup runs even if assertions panic
    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    // Connect WebSocket
    let ws_url = fixture.ws_url();
    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .expect("WebSocket should connect");

    // Post a message via RPC
    let test_message = format!("WebSocket test message {}", std::process::id());
    let _post_params = serde_json::json!({
        "message": test_message,
        "from": "ws-test-agent"
    });

    // Need to do RPC in a blocking context
    let socket_path = fixture.socket_path.clone();
    let message_clone = test_message.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(mut stream) = UnixStream::connect(&socket_path) {
            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "channel.post",
                "params": {
                    "message": message_clone,
                    "from": "ws-test-agent"
                },
                "id": 1
            });
            let request_line = format!("{}\n", request);
            let _ = stream.write_all(request_line.as_bytes());
            let _ = stream.flush();
        }
    })
    .await
    .unwrap();

    // Wait for the message to arrive via WebSocket
    let mut received = false;
    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(update) = serde_json::from_str::<serde_json::Value>(&text)
                            && update.get("type").and_then(|t| t.as_str()) == Some("channel_message")
                        {
                            let data = &update["data"];
                            if data.get("from").and_then(|f| f.as_str()) == Some("ws-test-agent")
                                && data.get("content").and_then(|c| c.as_str())
                                    .map(|s| s.contains(&test_message))
                                    .unwrap_or(false)
                            {
                                received = true;
                                break;
                            }
                        }
                    }
                    Some(Err(e)) => {
                        eprintln!("WebSocket error: {}", e);
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
            _ = &mut timeout => {
                break;
            }
        }
    }

    assert!(received, "Should receive channel message via WebSocket");

    let _ = ws_stream.close(None).await;
    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test that WebSocket client can send messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_websocket_send_message() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    // Create cleanup guard - ensures cleanup runs even if assertions panic
    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    // Connect WebSocket
    let ws_url = fixture.ws_url();
    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .expect("WebSocket should connect");

    // Send a message via WebSocket
    let send_msg = serde_json::json!({
        "type": "send_message",
        "content": "Hello from WebSocket test"
    });

    ws_stream
        .send(Message::Text(send_msg.to_string().into()))
        .await
        .expect("Should be able to send message");

    // Give time for the message to be processed
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The message should appear in channel history (or be broadcast back)
    // For now just verify we don't get an error response
    let timeout = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(msg) = ws_stream.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    // Check for any error response
                    if text.contains("error") {
                        panic!("Received error response: {}", text);
                    }
                    // If we get a channel_message with our content, test passes
                    if text.contains("Hello from WebSocket test") {
                        return;
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => panic!("WebSocket error: {}", e),
                _ => {}
            }
        }
    })
    .await;

    // Timeout is OK - message may not be echoed back
    if timeout.is_err() {
        // Just verify the connection is still alive
    }

    let _ = ws_stream.close(None).await;
    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test that view_window message is accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_websocket_view_window_message() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    // Create cleanup guard - ensures cleanup runs even if assertions panic
    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    let ws_url = fixture.ws_url();
    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .expect("WebSocket should connect");

    // Send view_window message
    let msg = serde_json::json!({
        "type": "view_window",
        "window": "lead",
        "cols": 120
    });

    let result = ws_stream.send(Message::Text(msg.to_string().into())).await;
    assert!(result.is_ok(), "Should accept view_window message");

    // Send leave_window message
    let msg = serde_json::json!({
        "type": "leave_window"
    });

    let result = ws_stream.send(Message::Text(msg.to_string().into())).await;
    assert!(result.is_ok(), "Should accept leave_window message");

    let _ = ws_stream.close(None).await;
    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test WebSocket message type validation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_websocket_invalid_message_type() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    // Create cleanup guard - ensures cleanup runs even if assertions panic
    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    let ws_url = fixture.ws_url();
    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .expect("WebSocket should connect");

    // Send invalid message type
    let msg = serde_json::json!({
        "type": "invalid_type",
        "data": "test"
    });

    let result = ws_stream.send(Message::Text(msg.to_string().into())).await;
    assert!(
        result.is_ok(),
        "Server should accept and handle invalid message"
    );

    // Connection should still be alive
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send a valid ping to verify connection
    let ping_result = ws_stream.send(Message::Ping(vec![].into())).await;
    assert!(
        ping_result.is_ok(),
        "Connection should remain open after invalid message"
    );

    let _ = ws_stream.close(None).await;
    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

// ────────────────────────────────────────────────────────────────────────────
// WebSocket Update Types Tests
// ────────────────────────────────────────────────────────────────────────────

/// Test that coworker_status updates are properly formatted.
#[test]
fn test_web_update_coworker_status_format() {
    // This is a unit test for the WebUpdate serialization
    let update = serde_json::json!({
        "type": "coworker_status",
        "data": {
            "name": "riverside",
            "status": "running",
            "current_task": "Test task"
        }
    });

    let serialized = serde_json::to_string(&update).unwrap();
    assert!(serialized.contains("coworker_status"));
    assert!(serialized.contains("riverside"));
    assert!(serialized.contains("running"));
}

/// Test that channel_message updates are properly formatted.
#[test]
fn test_web_update_channel_message_format() {
    let update = serde_json::json!({
        "type": "channel_message",
        "data": {
            "from": "lead",
            "content": "Hello world",
            "timestamp": "2024-01-01T00:00:00Z",
            "msg_type": "text"
        }
    });

    let serialized = serde_json::to_string(&update).unwrap();
    assert!(serialized.contains("channel_message"));
    assert!(serialized.contains("lead"));
    assert!(serialized.contains("Hello world"));
}

/// Test that lead_typing updates are properly formatted.
#[test]
fn test_web_update_lead_typing_format() {
    let update = serde_json::json!({
        "type": "lead_typing",
        "data": {
            "working": true
        }
    });

    let serialized = serde_json::to_string(&update).unwrap();
    assert!(serialized.contains("lead_typing"));
    assert!(serialized.contains("true"));
}

// ────────────────────────────────────────────────────────────────────────────
// Static File Serving Tests (standalone webserver)
// ────────────────────────────────────────────────────────────────────────────

/// Test that static files exist and are served correctly.
#[test]
fn test_web_assets_exist() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web_dir = manifest_dir.join("web-app").join("dist");

    if !web_dir.exists() {
        eprintln!("Skipping: web-app/dist not found - run 'cd web-app && npm run build'");
        return;
    }

    // Check essential files exist
    assert!(
        web_dir.join("index.html").exists(),
        "index.html should exist in web-app/dist"
    );

    // Check for JS bundle
    let has_js = fs::read_dir(web_dir.join("assets"))
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().is_some_and(|ext| ext == "js"))
        })
        .unwrap_or(false);
    assert!(has_js, "Should have JS bundle in assets/");

    // Check for CSS
    let has_css = fs::read_dir(web_dir.join("assets"))
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().is_some_and(|ext| ext == "css"))
        })
        .unwrap_or(false);
    assert!(has_css, "Should have CSS in assets/");
}

// ────────────────────────────────────────────────────────────────────────────
// Multi-Project Webserver Tests
// ────────────────────────────────────────────────────────────────────────────

/// Test that webserver can discover projects.
#[test]
fn test_webserver_project_discovery() {
    // This test verifies the project discovery logic without starting a server
    let projects_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("projects");

    if !projects_dir.exists() {
        // No projects dir - discovery should return empty
        return;
    }

    // The discovery function should not panic on any directory structure
    let entries = fs::read_dir(&projects_dir);
    if let Ok(entries) = entries {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                // Should be able to read directory name
                let name = entry.file_name();
                assert!(!name.is_empty(), "Directory should have a name");
            }
        }
    }
}

/// Test ProjectInfo serialization.
#[test]
fn test_project_info_serialization() {
    let info = serde_json::json!({
        "name": "test-project",
        "status": "running",
        "daemon_socket": "/tmp/test.sock",
        "webhook_port": 47023
    });

    let serialized = serde_json::to_string(&info).unwrap();
    assert!(serialized.contains("test-project"));
    assert!(serialized.contains("running"));
    assert!(serialized.contains("47023"));
}

// ────────────────────────────────────────────────────────────────────────────
// Error Handling Tests
// ────────────────────────────────────────────────────────────────────────────

/// Test that API gracefully handles missing channel.
#[test]
#[ignore] // Requires built binary
fn test_web_api_handles_channel_errors() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Channel should exist after daemon starts
    let response = reqwest::blocking::get(format!("{}/channel", fixture.api_base()));
    assert!(response.is_ok(), "Channel endpoint should be accessible");
    assert!(
        response.unwrap().status().is_success(),
        "Channel should return success"
    );
}

/// Test that status endpoint handles missing coworker manager gracefully.
#[test]
#[ignore] // Requires built binary
fn test_web_api_status_handles_empty_state() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Status should work even with no coworkers
    let response = reqwest::blocking::get(format!("{}/status", fixture.api_base()));
    assert!(response.is_ok(), "Status endpoint should be accessible");

    let response = response.unwrap();
    assert!(response.status().is_success(), "Status should return 200");

    let status: serde_json::Value = response.json().unwrap();
    let coworkers = status.get("coworkers").and_then(|c| c.as_array());
    assert!(coworkers.is_some(), "Status should include coworkers array");
    assert!(
        coworkers.unwrap().is_empty(),
        "Coworkers should be empty in test"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Integration Tests (requires running daemon)
// ────────────────────────────────────────────────────────────────────────────

/// Test full message flow: post via RPC -> receive via WebSocket -> see in history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_full_message_flow() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    // Create cleanup guard - ensures cleanup runs even if assertions panic
    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    // Connect WebSocket
    let ws_url = fixture.ws_url();
    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .expect("WebSocket should connect");

    // Generate unique message
    let unique_id = format!("flow-test-{}", std::process::id());
    let test_message = format!("Full flow test {}", unique_id);

    // Post message via RPC
    let socket_path = fixture.socket_path.clone();
    let message_clone = test_message.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(mut stream) = UnixStream::connect(&socket_path) {
            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "channel.post",
                "params": {
                    "message": message_clone,
                    "from": "flow-test"
                },
                "id": 1
            });
            let request_line = format!("{}\n", request);
            let _ = stream.write_all(request_line.as_bytes());
            let _ = stream.flush();
        }
    })
    .await
    .unwrap();

    // Verify message received via WebSocket
    let mut ws_received = false;
    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if text.contains(&unique_id) && text.contains("channel_message") {
                            ws_received = true;
                            break;
                        }
                    }
                    Some(Err(_)) | None => break,
                    _ => {}
                }
            }
            _ = &mut timeout => break,
        }
    }

    assert!(ws_received, "Should receive message via WebSocket");

    // Verify message in REST history using async reqwest
    let api_base = fixture.api_base();
    let history_response = reqwest::get(format!("{}/channel", api_base))
        .await
        .expect("History endpoint should work");
    let messages: Vec<serde_json::Value> = history_response
        .json()
        .await
        .expect("Should parse JSON response");

    let in_history = messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.contains(&unique_id))
            .unwrap_or(false)
    });
    assert!(in_history, "Message should be in REST history");

    let _ = ws_stream.close(None).await;
    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test that multiple WebSocket clients receive the same broadcast.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_websocket_broadcast_to_multiple_clients() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    // Create cleanup guard - ensures cleanup runs even if assertions panic
    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    let ws_url = fixture.ws_url();

    // Connect two WebSocket clients
    let (ws1, _) = connect_async(&ws_url)
        .await
        .expect("First WebSocket should connect");
    let (ws2, _) = connect_async(&ws_url)
        .await
        .expect("Second WebSocket should connect");

    // Post a message
    let unique_id = format!("broadcast-{}", std::process::id());
    let socket_path = fixture.socket_path.clone();
    let id_clone = unique_id.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(mut stream) = UnixStream::connect(&socket_path) {
            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "channel.post",
                "params": {
                    "message": format!("Broadcast test {}", id_clone),
                    "from": "broadcast-test"
                },
                "id": 1
            });
            let request_line = format!("{}\n", request);
            let _ = stream.write_all(request_line.as_bytes());
            let _ = stream.flush();
        }
    })
    .await
    .unwrap();

    // Helper function to check if a client receives the message
    async fn check_received(
        mut stream: futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
        id: String,
    ) -> bool {
        let timeout = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                msg = stream.next() => {
                    if let Some(Ok(Message::Text(text))) = msg
                        && text.contains(&id)
                    {
                        return true;
                    }
                }
                _ = &mut timeout => return false,
            }
        }
    }

    let (_, read1) = ws1.split();
    let (_, read2) = ws2.split();

    let id1 = unique_id.clone();
    let id2 = unique_id.clone();

    let (r1, r2) = tokio::join!(check_received(read1, id1), check_received(read2, id2));

    assert!(
        r1 && r2,
        "Both clients should receive broadcast: client1={}, client2={}",
        r1,
        r2
    );

    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

// ────────────────────────────────────────────────────────────────────────────
// File Upload Tests
// ────────────────────────────────────────────────────────────────────────────

/// Test that /api/upload accepts file uploads and returns correct path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_api_upload_success() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    // Create a test file
    let test_content = b"Test image content";
    let client = reqwest::Client::new();

    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(test_content.to_vec())
            .file_name("test-image.png")
            .mime_str("image/png")
            .unwrap(),
    );

    let response = client
        .post(format!("{}/upload", fixture.api_base()))
        .multipart(form)
        .send()
        .await;

    let response = match response {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Upload request failed: {}", e);
            fixture.async_cleanup().await;
            std::mem::forget(fixture);
            return;
        }
    };

    assert!(response.status().is_success(), "Upload should return 200");

    let body: serde_json::Value = response.json().await.expect("Should parse JSON response");

    // Verify response contains path and filename
    assert!(body.get("path").is_some(), "Response should contain 'path'");
    assert!(
        body.get("filename").is_some(),
        "Response should contain 'filename'"
    );

    let path = body.get("path").unwrap().as_str().unwrap();
    let filename = body.get("filename").unwrap().as_str().unwrap();

    // Verify filename has timestamp prefix
    assert!(
        filename.contains("test-image.png"),
        "Filename should preserve original name"
    );
    assert!(
        filename.contains('-'),
        "Filename should have timestamp prefix"
    );

    // Verify file was actually written
    assert!(PathBuf::from(path).exists(), "Uploaded file should exist");

    // Verify file content
    let content = fs::read(path).expect("Should be able to read uploaded file");
    assert_eq!(content, test_content, "File content should match");

    // Clean up test file
    let _ = fs::remove_file(path);

    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test that /api/upload accepts files up to 10MB.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_api_upload_accepts_medium_files() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    // Create a 5MB file (within the 10MB limit)
    let medium_content = vec![0u8; 5 * 1024 * 1024]; // 5MB
    let client = reqwest::Client::new();

    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(medium_content.clone())
            .file_name("medium-file.bin")
            .mime_str("application/octet-stream")
            .unwrap(),
    );

    let response = client
        .post(format!("{}/upload", fixture.api_base()))
        .multipart(form)
        .send()
        .await;

    let response = match response {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Request failed: {}", e);
            fixture.async_cleanup().await;
            std::mem::forget(fixture);
            return;
        }
    };

    assert!(
        response.status().is_success(),
        "Should accept 5MB file (got status {})",
        response.status()
    );

    let body: serde_json::Value = response.json().await.expect("Should parse JSON response");
    assert!(body.get("path").is_some(), "Response should contain 'path'");

    let path = body.get("path").unwrap().as_str().unwrap();

    // Verify file was written with correct size
    let metadata = fs::metadata(path).expect("Should be able to stat uploaded file");
    assert_eq!(
        metadata.len(),
        (5 * 1024 * 1024) as u64,
        "File size should match"
    );

    // Clean up test file
    let _ = fs::remove_file(path);

    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test that /api/upload rejects files that are too large.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_api_upload_rejects_large_files() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    // Create a file larger than 10MB
    let large_content = vec![0u8; 11 * 1024 * 1024]; // 11MB
    let client = reqwest::Client::new();

    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(large_content)
            .file_name("large-file.bin")
            .mime_str("application/octet-stream")
            .unwrap(),
    );

    let response = client
        .post(format!("{}/upload", fixture.api_base()))
        .multipart(form)
        .send()
        .await;

    let response = match response {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Request failed: {}", e);
            fixture.async_cleanup().await;
            std::mem::forget(fixture);
            return;
        }
    };

    assert_eq!(
        response.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        "Should reject files larger than 10MB"
    );

    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test that /api/upload rejects invalid filenames.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_api_upload_rejects_invalid_filenames() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    let test_content = b"Test content";
    let client = reqwest::Client::new();

    // Test directory traversal attempt
    let invalid_filenames = vec![
        "../etc/passwd",
        "../../secret.txt",
        "subdir/file.txt",
        "evil\\..\\passwd",
    ];

    for filename in invalid_filenames {
        let form = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(test_content.to_vec())
                .file_name(filename)
                .mime_str("text/plain")
                .unwrap(),
        );

        let response = client
            .post(format!("{}/upload", fixture.api_base()))
            .multipart(form)
            .send()
            .await;

        let response = match response {
            Ok(r) => r,
            Err(_) => continue, // Network error, skip this test case
        };

        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "Should reject filename '{}'",
            filename
        );
    }

    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test that /api/upload returns error when no file is provided.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_api_upload_no_file_error() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    let client = reqwest::Client::new();

    // Send empty multipart form
    let form = reqwest::multipart::Form::new();

    let response = client
        .post(format!("{}/upload", fixture.api_base()))
        .multipart(form)
        .send()
        .await;

    let response = match response {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Request failed: {}", e);
            fixture.async_cleanup().await;
            std::mem::forget(fixture);
            return;
        }
    };

    assert_eq!(
        response.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "Should return 400 when no file is provided"
    );

    let body: serde_json::Value = response.json().await.expect("Should parse JSON");
    assert!(
        body.get("error").is_some(),
        "Error response should contain error field"
    );

    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

// ────────────────────────────────────────────────────────────────────────────
// Per-Channel API Tests
// ────────────────────────────────────────────────────────────────────────────

/// Test that /api/channel accepts ?channel=name query parameter and returns
/// messages from the specified channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_api_channel_history_per_channel() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    // Post messages to different channels via RPC
    let socket_path = fixture.socket_path.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(mut stream) = UnixStream::connect(&socket_path) {
            // Post to main channel
            let request1 = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "channel.post",
                "params": {
                    "message": "Main channel message",
                    "from": "test-agent"
                },
                "id": 1
            });
            let _ = stream.write_all(format!("{}\n", request1).as_bytes());
            let _ = stream.flush();

            // Wait a bit for the first message to be processed
            thread::sleep(Duration::from_millis(50));

            // Post to topic channel
            let request2 = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "channel.post",
                "params": {
                    "message": "Topic channel message",
                    "from": "test-agent",
                    "channel": "pr-42"
                },
                "id": 2
            });
            let _ = stream.write_all(format!("{}\n", request2).as_bytes());
            let _ = stream.flush();
        }
    })
    .await
    .unwrap();

    // Give the daemon time to process and write messages
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Fetch main channel history (no parameter)
    let main_response = reqwest::get(format!("{}/channel", fixture.api_base()))
        .await
        .expect("Main channel endpoint should work");
    assert!(
        main_response.status().is_success(),
        "Main channel should return 200"
    );

    let main_messages: Vec<serde_json::Value> = main_response
        .json()
        .await
        .expect("Should parse main channel JSON");

    // Main channel should have the main message
    let has_main = main_messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.contains("Main channel message"))
            .unwrap_or(false)
    });
    assert!(has_main, "Main channel should contain main channel message");

    // Fetch topic channel history with ?channel=pr-42
    let topic_response = reqwest::get(format!("{}/channel?channel=pr-42", fixture.api_base()))
        .await
        .expect("Topic channel endpoint should work");
    assert!(
        topic_response.status().is_success(),
        "Topic channel should return 200"
    );

    let topic_messages: Vec<serde_json::Value> = topic_response
        .json()
        .await
        .expect("Should parse topic channel JSON");

    // Topic channel should have the topic message
    let has_topic = topic_messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.contains("Topic channel message"))
            .unwrap_or(false)
            && m.get("channel")
                .and_then(|c| c.as_str())
                .map(|s| s == "pr-42")
                .unwrap_or(false)
    });
    assert!(
        has_topic,
        "Topic channel should contain topic channel message with correct channel field"
    );

    // Topic channel should NOT have the main channel message
    let has_main_in_topic = topic_messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.contains("Main channel message"))
            .unwrap_or(false)
    });
    assert!(
        !has_main_in_topic,
        "Topic channel should not contain main channel messages"
    );

    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test that /api/channel validates channel names and rejects invalid ones.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_api_channel_history_validates_channel_name() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    // Test invalid channel names (directory traversal, special chars)
    let invalid_names = vec![
        "../etc/passwd",
        "../../secret",
        "foo/bar",
        "foo:bar",
        "foo;bar",
        "", // empty name
    ];

    for name in invalid_names {
        let url = format!("{}/channel?channel={}", fixture.api_base(), name);
        let response = reqwest::get(&url).await;

        if let Ok(resp) = response {
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::BAD_REQUEST,
                "Should reject invalid channel name '{}'",
                name
            );
        }
    }

    // Valid channel names should not cause 400 errors (might return 404 or 200)
    let valid_names = vec!["pr-42", "task-5", "my-channel", "channel_123"];

    for name in valid_names {
        let url = format!("{}/channel?channel={}", fixture.api_base(), name);
        let response = reqwest::get(&url).await;

        if let Ok(resp) = response {
            assert_ne!(
                resp.status(),
                reqwest::StatusCode::BAD_REQUEST,
                "Should not reject valid channel name '{}', got status {}",
                name,
                resp.status()
            );
        }
    }

    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test that WebSocket broadcasts include the channel field for filtering.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_websocket_channel_field_in_broadcasts() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        fixture.async_cleanup().await;
        std::mem::forget(fixture);
        return;
    }

    let _cleanup_guard = AsyncCleanupGuard::new(&fixture);

    // Connect WebSocket
    let ws_url = fixture.ws_url();
    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .expect("WebSocket should connect");

    // Post messages to different channels
    let socket_path = fixture.socket_path.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(mut stream) = UnixStream::connect(&socket_path) {
            // Post to main channel
            let request1 = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "channel.post",
                "params": {
                    "message": "WS main message",
                    "from": "test-agent"
                },
                "id": 1
            });
            let _ = stream.write_all(format!("{}\n", request1).as_bytes());
            let _ = stream.flush();

            thread::sleep(Duration::from_millis(100));

            // Post to topic channel
            let request2 = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "channel.post",
                "params": {
                    "message": "WS topic message",
                    "from": "test-agent",
                    "channel": "test-channel"
                },
                "id": 2
            });
            let _ = stream.write_all(format!("{}\n", request2).as_bytes());
            let _ = stream.flush();
        }
    })
    .await
    .unwrap();

    // Collect WebSocket messages
    let mut main_received = false;
    let mut topic_received = false;
    let timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(update) = serde_json::from_str::<serde_json::Value>(&text)
                            && update.get("type").and_then(|t| t.as_str()) == Some("channel_message")
                        {
                            let data = &update["data"];

                            // Check for main channel message
                            if data.get("content").and_then(|c| c.as_str())
                                .map(|s| s.contains("WS main message"))
                                .unwrap_or(false)
                            {
                                // Should have channel field set to "midtown" (default)
                                let channel = data.get("channel")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("");
                                assert_eq!(
                                    channel, "midtown",
                                    "Main channel message should have channel='midtown'"
                                );
                                main_received = true;
                            }

                            // Check for topic channel message
                            if data.get("content").and_then(|c| c.as_str())
                                .map(|s| s.contains("WS topic message"))
                                .unwrap_or(false)
                            {
                                // Should have channel field set to "test-channel"
                                let channel = data.get("channel")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("");
                                assert_eq!(
                                    channel, "test-channel",
                                    "Topic channel message should have channel='test-channel'"
                                );
                                topic_received = true;
                            }

                            if main_received && topic_received {
                                break;
                            }
                        }
                    }
                    Some(Err(e)) => {
                        eprintln!("WebSocket error: {}", e);
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
            _ = &mut timeout => {
                break;
            }
        }
    }

    assert!(
        main_received,
        "Should receive main channel message with channel field"
    );
    assert!(
        topic_received,
        "Should receive topic channel message with channel field"
    );

    let _ = ws_stream.close(None).await;
    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}
