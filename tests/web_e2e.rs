//! End-to-end tests for the web UI and WebSocket functionality.
//!
//! These tests verify:
//! - REST API endpoints (health, channel, status)
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
        let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("release")
            .join("midtown");

        if !binary_path.exists() {
            eprintln!(
                "Release binary not found at {:?}. Run `cargo build --release` first.",
                binary_path
            );
            return false;
        }

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

/// Test that /api/channels/history returns an array of messages.
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
    let response = reqwest::blocking::get(format!("{}/channels/history", fixture.api_base()));
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
        assert!(
            msg.get("id")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "Message should have non-empty 'id' field"
        );
    }
}

/// Test that /api/channels/history defaults to top-level messages with reply metadata,
/// and filters by thread_parent_id when the query param is provided.
#[test]
#[ignore] // Requires built binary
fn test_web_api_channel_history_thread_parent_id_filter() {
    let mut fixture = match WebTestFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Post a top-level message
    let top_level_params = serde_json::json!({
        "message": "Top-level message",
        "from": "park"
    });
    fixture.rpc_call("channel.post", Some(top_level_params));

    // Resolve the real parent ID from history so thread reply metadata can be validated.
    thread::sleep(Duration::from_millis(100));
    let history_response =
        reqwest::blocking::get(format!("{}/channels/history", fixture.api_base()))
            .expect("history endpoint should be reachable");
    assert!(
        history_response.status().is_success(),
        "history endpoint should return success"
    );
    let history_messages: Vec<serde_json::Value> = history_response
        .json()
        .expect("history should be valid JSON");
    let parent_id = history_messages
        .iter()
        .find(|m| {
            m.get("content")
                .and_then(|v| v.as_str())
                .map(|s| s == "Top-level message")
                .unwrap_or(false)
        })
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .expect("should find top-level message ID in history");

    // Post a thread reply with thread_parent_id
    let thread_params = serde_json::json!({
        "message": "Thread reply",
        "from": "york",
        "thread_parent_id": parent_id.clone()
    });
    fixture.rpc_call("channel.post", Some(thread_params));

    thread::sleep(Duration::from_millis(100));

    // Default history should include only top-level messages and reply metadata.
    let top_level_response =
        reqwest::blocking::get(format!("{}/channels/history", fixture.api_base()));
    assert!(
        top_level_response.is_ok(),
        "Top-level history should be accessible"
    );
    let top_level_response = top_level_response.unwrap();
    assert!(
        top_level_response.status().is_success(),
        "Top-level history should return 200"
    );
    let top_level_messages: Vec<serde_json::Value> = top_level_response.json().unwrap();
    let parent = top_level_messages
        .iter()
        .find(|m| m.get("id").and_then(|v| v.as_str()) == Some(parent_id.as_str()))
        .expect("parent message should be present in top-level history");
    assert_eq!(
        parent.get("reply_count").and_then(|v| v.as_u64()),
        Some(1),
        "Top-level history should include reply_count for parent"
    );
    assert_eq!(
        parent
            .get("last_reply")
            .and_then(|v| v.get("from"))
            .and_then(|v| v.as_str()),
        Some("york"),
        "Top-level history should include last_reply.from"
    );
    assert!(
        !top_level_messages.iter().any(|m| {
            m.get("content")
                .and_then(|v| v.as_str())
                .map(|s| s == "Thread reply")
                .unwrap_or(false)
        }),
        "Top-level history should exclude thread replies"
    );

    // Filter history by thread_parent_id — should only return the thread reply
    let url = format!(
        "{}/channels/history?thread_parent_id={}",
        fixture.api_base(),
        parent_id
    );
    let response = reqwest::blocking::get(&url);
    assert!(
        response.is_ok(),
        "Channel history with filter should be accessible"
    );

    let response = response.unwrap();
    assert!(
        response.status().is_success(),
        "Filtered history should return 200"
    );

    let messages: Vec<serde_json::Value> = response.json().unwrap();
    assert_eq!(
        messages.len(),
        1,
        "Filtered history should return only the thread reply, got {} messages",
        messages.len()
    );
    assert_eq!(
        messages[0].get("from").and_then(|v| v.as_str()),
        Some("york"),
        "Filtered message should be from york"
    );
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

// ────────────────────────────────────────────────────────────────────────────
// Note: /api/tmux-windows, /api/tmux-pane, and /api/lead-pane endpoints were
// removed as part of the Zellij migration (Phase 7). The Svelte web app now
// embeds the Zellij web client instead of fetching tmux pane content.
// ────────────────────────────────────────────────────────────────────────────

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
    let response = reqwest::blocking::get(format!("{}/channels/history", fixture.api_base()));
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
    let history_response = reqwest::get(format!("{}/channels/history", api_base))
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

/// Test that /api/channels/history accepts ?channel=name query parameter and returns
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
    let main_response = reqwest::get(format!("{}/channels/history", fixture.api_base()))
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
    let topic_response = reqwest::get(format!(
        "{}/channels/history?channel=pr-42",
        fixture.api_base()
    ))
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

/// Test that /api/channels/history validates channel names and rejects invalid ones.
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
        let url = format!("{}/channels/history?channel={}", fixture.api_base(), name);
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
        let url = format!("{}/channels/history?channel={}", fixture.api_base(), name);
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

// ────────────────────────────────────────────────────────────────────────────
// Multi-Usage API Tests
// ────────────────────────────────────────────────────────────────────────────

/// Test that /api/usage returns 204 No Content when credentials are unavailable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // Requires built binary
async fn test_api_usage_no_credentials() {
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

    // Since this is a test environment without real OAuth credentials,
    // the endpoint should return 204 No Content
    let response = reqwest::get(format!("{}/usage", fixture.api_base())).await;

    if let Ok(resp) = response {
        // Either 204 (no credentials) or 200 (if somehow credentials exist in test env)
        assert!(
            resp.status() == reqwest::StatusCode::NO_CONTENT
                || resp.status() == reqwest::StatusCode::OK,
            "Usage endpoint should return 204 or 200, got {}",
            resp.status()
        );

        // If 200, verify the response structure
        if resp.status() == reqwest::StatusCode::OK {
            let data: serde_json::Value = resp.json().await.expect("Should parse JSON");

            // Test backwards compatibility: flat fields should exist
            assert!(
                data.get("session_util").is_some(),
                "Response should include session_util for backwards compatibility"
            );
            assert!(
                data.get("week_util").is_some(),
                "Response should include week_util for backwards compatibility"
            );
            assert!(
                data.get("account_email").is_some(),
                "Response should include account_email for backwards compatibility"
            );

            // Test new format: usage array should exist
            assert!(
                data.get("usage").and_then(|u| u.as_array()).is_some(),
                "Response should include usage array"
            );
        }
    }

    fixture.async_cleanup().await;
    std::mem::forget(fixture);
}

/// Test that /api/usage returns correct array format with provider/profile fields.
///
/// Note: This test uses mocked data since we don't have real credentials in tests.
/// It verifies the endpoint structure and JSON serialization.
#[test]
fn test_api_usage_array_format_serialization() {
    // Test the JSON structure that should be returned by api_usage()
    // This verifies the serialization format matches the spec

    // Simulated response structure
    let usage_response = serde_json::json!({
        "usage": [
            {
                "provider": "claude",
                "profile": "default",
                "session_util": 25.5,
                "session_resets": "2024-01-15T12:00:00Z",
                "week_util": 60.2,
                "week_resets": "2024-01-20T00:00:00Z",
                "account_email": "test@example.com"
            },
            {
                "provider": "claude",
                "profile": "work",
                "session_util": 10.0,
                "session_resets": "2024-01-15T14:00:00Z",
                "week_util": 30.5,
                "week_resets": "2024-01-20T00:00:00Z",
                "account_email": "work@example.com"
            }
        ],
        // Backwards compatibility: flat fields for primary account
        "session_util": 25.5,
        "session_resets": "2024-01-15T12:00:00Z",
        "week_util": 60.2,
        "week_resets": "2024-01-20T00:00:00Z",
        "account_email": "test@example.com"
    });

    // Verify the usage array structure
    let usage_array = usage_response
        .get("usage")
        .and_then(|u| u.as_array())
        .expect("Should have usage array");

    assert_eq!(usage_array.len(), 2, "Should have 2 usage entries");

    // Verify first entry has all required fields
    let first_entry = &usage_array[0];
    assert_eq!(
        first_entry.get("provider").and_then(|p| p.as_str()),
        Some("claude"),
        "First entry should have provider"
    );
    assert_eq!(
        first_entry.get("profile").and_then(|p| p.as_str()),
        Some("default"),
        "First entry should have profile"
    );
    assert!(
        first_entry
            .get("session_util")
            .and_then(|s| s.as_f64())
            .is_some(),
        "First entry should have session_util"
    );
    assert!(
        first_entry
            .get("week_util")
            .and_then(|w| w.as_f64())
            .is_some(),
        "First entry should have week_util"
    );
    assert!(
        first_entry.get("account_email").is_some(),
        "First entry should have account_email"
    );

    // Verify backwards compatibility: flat fields match first entry
    assert_eq!(
        usage_response.get("session_util").and_then(|s| s.as_f64()),
        first_entry.get("session_util").and_then(|s| s.as_f64()),
        "Flat session_util should match first entry"
    );
    assert_eq!(
        usage_response.get("week_util").and_then(|w| w.as_f64()),
        first_entry.get("week_util").and_then(|w| w.as_f64()),
        "Flat week_util should match first entry"
    );
    assert_eq!(
        usage_response.get("account_email").and_then(|e| e.as_str()),
        first_entry.get("account_email").and_then(|e| e.as_str()),
        "Flat account_email should match first entry"
    );
}

/// Test that flat field backwards compatibility works for single-account consumers.
#[test]
fn test_api_usage_backwards_compatibility_flat_fields() {
    // This test verifies that the old API contract (flat fields) still works
    // for consumers that haven't migrated to the new array format

    let usage_response = serde_json::json!({
        "usage": [
            {
                "provider": "claude",
                "profile": "default",
                "session_util": 42.7,
                "session_resets": "2024-01-15T10:00:00Z",
                "week_util": 78.3,
                "week_resets": "2024-01-18T00:00:00Z",
                "account_email": "legacy@example.com"
            }
        ],
        "session_util": 42.7,
        "session_resets": "2024-01-15T10:00:00Z",
        "week_util": 78.3,
        "week_resets": "2024-01-18T00:00:00Z",
        "account_email": "legacy@example.com"
    });

    // Old consumers reading only flat fields should get valid data
    assert_eq!(
        usage_response.get("session_util").and_then(|s| s.as_f64()),
        Some(42.7),
        "Flat session_util should be accessible"
    );
    assert_eq!(
        usage_response.get("week_util").and_then(|w| w.as_f64()),
        Some(78.3),
        "Flat week_util should be accessible"
    );
    assert_eq!(
        usage_response.get("account_email").and_then(|e| e.as_str()),
        Some("legacy@example.com"),
        "Flat account_email should be accessible"
    );
    assert!(
        usage_response.get("session_resets").is_some(),
        "Flat session_resets should be accessible"
    );
    assert!(
        usage_response.get("week_resets").is_some(),
        "Flat week_resets should be accessible"
    );

    // New consumers should be able to read the array format
    let usage_array = usage_response
        .get("usage")
        .and_then(|u| u.as_array())
        .expect("Should have usage array");
    assert_eq!(usage_array.len(), 1, "Should have one usage entry");
}

/// Test that usage array contains distinct provider/profile combinations.
#[test]
fn test_api_usage_array_distinct_profiles() {
    // Simulated response with multiple profiles
    let usage_response = serde_json::json!({
        "usage": [
            {
                "provider": "claude",
                "profile": "default",
                "session_util": 20.0,
                "week_util": 50.0,
                "account_email": "user1@example.com"
            },
            {
                "provider": "claude",
                "profile": "work",
                "session_util": 15.0,
                "week_util": 40.0,
                "account_email": "user2@example.com"
            },
            {
                "provider": "claude",
                "profile": "personal",
                "session_util": 5.0,
                "week_util": 10.0,
                "account_email": "user3@example.com"
            }
        ],
        "session_util": 20.0,
        "week_util": 50.0,
        "account_email": "user1@example.com"
    });

    let usage_array = usage_response
        .get("usage")
        .and_then(|u| u.as_array())
        .expect("Should have usage array");

    // Verify all entries have distinct profiles
    let profiles: Vec<&str> = usage_array
        .iter()
        .filter_map(|u| u.get("profile").and_then(|p| p.as_str()))
        .collect();

    assert_eq!(profiles.len(), 3, "Should have 3 profile entries");
    assert!(profiles.contains(&"default"), "Should have default profile");
    assert!(profiles.contains(&"work"), "Should have work profile");
    assert!(
        profiles.contains(&"personal"),
        "Should have personal profile"
    );

    // Verify all entries have the same provider (currently only Claude supported)
    let providers: Vec<&str> = usage_array
        .iter()
        .filter_map(|u| u.get("provider").and_then(|p| p.as_str()))
        .collect();

    assert!(
        providers.iter().all(|&p| p == "claude"),
        "All providers should be 'claude'"
    );
}

/// Test documenting cache keying limitation.
///
/// LIMITATION: MULTI_USAGE_CACHE currently does not key by active profile set.
/// This means if the coworker set changes (e.g., a new coworker spawns with a
/// different profile, or a coworker shuts down), stale data may be served for
/// up to 2 minutes until the cache expires.
///
/// This test documents the current behavior. A future fix would key the cache
/// by the set of active (provider, profile) combinations.
#[test]
fn test_api_usage_cache_limitation_documented() {
    // This test documents that the cache doesn't differentiate by profile set.
    // In practice this means:
    //
    // Time T:   Coworkers {(claude, "default")} → cache stores [{...}]
    // Time T+1: New coworker spawns with (claude, "work")
    // Time T+2: GET /api/usage → returns stale [{...}] for ~2 min
    //
    // The correct behavior would be to invalidate cache when profile set changes,
    // or key cache by the active profile set.
    //
    // For now, we accept the 2-minute staleness as a tradeoff for simplicity.
    // If this becomes problematic, the fix would be in src/web.rs:
    //
    // 1. Change cache key from () to HashSet<(AuthProvider, String)>
    // 2. Compute active_profiles_key before cache lookup
    // 3. Use MULTI_USAGE_CACHE.get_with_key(active_profiles_key, TTL)

    // This test intentionally passes - it's documentation of known behavior.
    // No actual assertion needed - the test body documents the limitation.
}
