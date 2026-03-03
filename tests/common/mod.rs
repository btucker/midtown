//! Shared test infrastructure for E2E tests.
//!
//! This module provides reusable test fixtures and helpers used across
//! daemon_e2e.rs, webhook_e2e.rs, and webhook_effect_pipeline_e2e.rs.
//!
//! Not all consumers use all items, so we allow dead_code at the module level.
#![allow(dead_code)]

use std::cell::Cell;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

// ── Test Naming and Cleanup ─────────────────────────────────────────

/// Counter for unique test names across tests.
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique test repo name to avoid conflicts.
pub fn test_repo_name(prefix: &str) -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{}-{}-{}", prefix, std::process::id(), counter)
}

/// Kill any orphaned daemon processes matching the given pattern.
///
/// This is a safety measure to ensure tests don't interfere with each other
/// if a previous test run crashed without cleaning up properly.
pub fn cleanup_orphaned_test_daemons(pattern: &str) {
    let _ = Command::new("pkill")
        .args(["-f", &format!("midtown daemon.*{}", pattern)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    thread::sleep(Duration::from_millis(100));

    // Clean up stale project directories from crashed previous runs.
    // Skip directories from the current process to avoid interfering with
    // concurrently running tests in the same process.
    let current_pid = format!("{}-{}-", pattern, std::process::id());
    let projects_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("projects");
    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with(pattern)
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    // Also clean up stale socket directories (same PID filter)
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
                && name.starts_with(pattern)
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

// ── Git Repository Setup ────────────────────────────────────────────

/// Initialize a git repository in the given directory.
///
/// Creates a git repo with initial commit and configured user.
/// Returns true on success.
pub fn init_git_repo(dir: &PathBuf) -> bool {
    // Initialize repo
    let status = Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !status.is_ok_and(|s| s.success()) {
        return false;
    }

    // Configure git user
    let _ = Command::new("git")
        .args(["config", "user.email", "test@midtown.local"])
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("git")
        .args(["config", "user.name", "Midtown Test"])
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    // Create initial commit
    let status = Command::new("git")
        .args(["commit", "--allow-empty", "-m", "Initial commit"])
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    status.is_ok_and(|s| s.success())
}

// ── Daemon Test Harness ─────────────────────────────────────────────

/// Options for configuring a DaemonTestHarness.
#[derive(Default)]
pub struct DaemonHarnessOptions {
    /// Enable webhook server (requires webhook_port)
    pub enable_webhook: bool,
    /// Webhook server port (only used if enable_webhook is true)
    pub webhook_port: u16,
    /// Webhook secret for HMAC signing (only used if enable_webhook is true)
    pub webhook_secret: String,
    /// Custom XDG_STATE_HOME to avoid socket path length issues
    pub custom_state_dir: Option<PathBuf>,
}

/// Test harness managing daemon lifecycle and providing test utilities.
///
/// This struct handles:
/// - Creating an isolated git repository
/// - Starting/stopping the daemon process
/// - Connecting to the daemon via RPC
/// - Sending webhooks (if webhook enabled)
/// - Reading channel messages
/// - Cleaning up resources on drop
pub struct DaemonTestHarness {
    /// Temporary directory containing the test repo
    pub temp_dir: PathBuf,
    /// Project directory under ~/.midtown/projects/<name>/
    pub project_dir: PathBuf,
    /// Repository name (used for socket path derivation and cleanup)
    pub repo_name: String,
    /// Path to the daemon socket
    pub socket_path: PathBuf,
    /// Per-fixture state directory (used as XDG_STATE_HOME)
    pub state_dir: PathBuf,
    /// Path to the daemon PID file
    pub pid_path: PathBuf,
    /// Task directory for this test repo (~/.claude/tasks/midtown-<repo>/)
    pub tasks_dir: PathBuf,
    /// Webhook configuration (if enabled)
    pub webhook_port: Option<u16>,
    pub webhook_secret: Option<String>,
    /// Daemon process handle (if started)
    daemon_process: Option<Child>,
    /// Request ID counter for generating unique RPC request IDs
    next_request_id: Cell<u64>,
}

impl DaemonTestHarness {
    /// Create a new test harness with the given repo name prefix.
    ///
    /// The prefix is used to generate a unique repo name and for cleanup.
    /// Returns None if git initialization fails.
    pub fn new(prefix: &str, options: DaemonHarnessOptions) -> Option<Self> {
        cleanup_orphaned_test_daemons(prefix);

        let repo_name = test_repo_name(prefix);
        let temp_dir = std::env::temp_dir().join(&repo_name);

        // Clean up any previous test data
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).ok()?;

        // Initialize git repository
        if !init_git_repo(&temp_dir) {
            return None;
        }

        // Compute state directory
        let state_dir = options.custom_state_dir.unwrap_or_else(|| {
            std::env::var("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    dirs::home_dir()
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join(".local")
                        .join("state")
                })
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

        let tasks_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("tasks")
            .join(format!("midtown-{}", &repo_name));

        // Ensure parent directories exist
        if let Some(parent) = socket_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Some(parent) = pid_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let (webhook_port, webhook_secret) = if options.enable_webhook {
            (Some(options.webhook_port), Some(options.webhook_secret))
        } else {
            (None, None)
        };

        Some(Self {
            temp_dir,
            project_dir,
            repo_name,
            socket_path,
            state_dir,
            pid_path,
            tasks_dir,
            webhook_port,
            webhook_secret,
            daemon_process: None,
            next_request_id: Cell::new(1),
        })
    }

    /// Start the daemon process.
    ///
    /// Returns true if the daemon started successfully and the socket is available.
    pub fn start_daemon(&mut self) -> bool {
        self.start_daemon_with_timeout(60000)
    }

    /// Start the daemon process with a custom timeout in milliseconds.
    pub fn start_daemon_with_timeout(&mut self, timeout_ms: u64) -> bool {
        let binary_path = self.find_midtown_binary();
        if binary_path.is_none() {
            eprintln!("No midtown binary found. Run `cargo build` first.");
            return false;
        }
        let binary_path = binary_path.unwrap();

        // Remove stale socket/pid if present
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.pid_path);

        // Prepare log files
        let log_path = self.temp_dir.join("daemon.log");
        let err_path = self.temp_dir.join("daemon_err.log");
        let log_file = fs::File::create(&log_path).ok();
        let log_err = fs::File::create(&err_path).ok();

        // Build daemon command
        let mut cmd = Command::new(&binary_path);
        cmd.arg("daemon")
            .arg("--workdir")
            .arg(&self.temp_dir)
            .current_dir(&self.temp_dir)
            .env("MIDTOWN_CHAT_MONITOR", "0") // Disable for tests
            .env("XDG_STATE_HOME", &self.state_dir)
            .stdout(log_file.map(Stdio::from).unwrap_or(Stdio::null()))
            .stderr(log_err.map(Stdio::from).unwrap_or(Stdio::null()));

        // Configure webhook if enabled
        if let Some(port) = self.webhook_port {
            cmd.env("MIDTOWN_WEBHOOK_PORT", port.to_string());
            if let Some(ref secret) = self.webhook_secret {
                cmd.env("MIDTOWN_WEBHOOK_SECRET", secret);
            }
            cmd.env("RUST_LOG", "midtown=debug");
        } else {
            cmd.env("MIDTOWN_WEBHOOK_PORT", "0");
        }

        // Spawn daemon
        let child = cmd.spawn();

        match child {
            Ok(child) => {
                self.daemon_process = Some(child);

                // Wait for socket to become available
                let max_attempts = timeout_ms / 100;
                for i in 0..max_attempts {
                    thread::sleep(Duration::from_millis(100));

                    if self.socket_path.exists() && UnixStream::connect(&self.socket_path).is_ok() {
                        // For webhook tests, wait a bit more for HTTP server
                        if self.webhook_port.is_some() {
                            thread::sleep(Duration::from_millis(500));
                        }
                        return true;
                    }

                    // Check if daemon exited early with error
                    if let Some(ref mut proc) = self.daemon_process
                        && let Ok(Some(status)) = proc.try_wait()
                        && !status.success()
                    {
                        eprintln!("Daemon exited early with error status: {:?}", status);
                        self.print_daemon_logs(&log_path, &err_path);
                        return false;
                    }

                    if i == max_attempts / 4 {
                        eprintln!("Waiting for socket... ({} attempts)", i);
                    }
                }

                eprintln!("Daemon socket never appeared at {:?}", self.socket_path);
                self.print_daemon_logs(&log_path, &err_path);
                false
            }
            Err(e) => {
                eprintln!("Failed to spawn daemon: {}", e);
                false
            }
        }
    }

    fn find_midtown_binary(&self) -> Option<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut candidates = Vec::new();

        if let Some(bin) = option_env!("CARGO_BIN_EXE_midtown") {
            candidates.push(PathBuf::from(bin));
        }
        candidates.extend([
            manifest_dir.join("target/debug/midtown"),
            manifest_dir.join("target/llvm-cov-target/debug/midtown"),
            manifest_dir.join("target/release/midtown"),
        ]);

        candidates.iter().find(|p| p.exists()).cloned()
    }

    fn print_daemon_logs(&self, log_path: &PathBuf, err_path: &PathBuf) {
        if let Ok(log) = fs::read_to_string(log_path) {
            eprintln!("Daemon stdout:\n{}", log);
        }
        if let Ok(err) = fs::read_to_string(err_path) {
            eprintln!("Daemon stderr:\n{}", err);
        }
    }

    /// Connect to the daemon socket.
    pub fn connect(&self) -> Option<UnixStream> {
        UnixStream::connect(&self.socket_path).ok()
    }

    /// Make an RPC call to the daemon.
    ///
    /// Returns the JSON-RPC response, or None on error.
    pub fn rpc_call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        self.rpc_call_with_timeout(method, params, Duration::from_secs(30))
    }

    /// Make an RPC call with a custom timeout.
    ///
    /// Returns None if the response doesn't arrive within the timeout.
    pub fn rpc_call_with_timeout(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        let id = self.next_request_id.get();
        self.next_request_id.set(id + 1);

        let mut stream = self.connect()?;

        stream.set_read_timeout(Some(timeout)).ok()?;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id
        });

        let request_line = format!("{}\n", request);
        stream.write_all(request_line.as_bytes()).ok()?;
        stream.flush().ok()?;

        let mut reader = BufReader::new(&stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).ok()?;

        serde_json::from_str(&response_line).ok()
    }

    /// Stop the daemon gracefully.
    ///
    /// Attempts RPC shutdown first, then kills the process, then uses pkill
    /// as a final fallback.
    pub fn stop_daemon(&mut self) {
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

        // As a final fallback, use pkill to ensure any daemon for this repo is stopped
        let pattern = format!("midtown daemon.*{}", self.repo_name);
        let _ = Command::new("pkill")
            .args(["-f", &pattern])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// Read recent messages from the channel via RPC.
    pub fn read_channel_messages(&self) -> Vec<String> {
        self.read_channel_messages_with_limit(100)
    }

    /// Read recent messages from the channel with a custom limit.
    pub fn read_channel_messages_with_limit(&self, limit: u64) -> Vec<String> {
        let response = self.rpc_call("channel.read", Some(serde_json::json!({"limit": limit})));

        if let Some(response) = response
            && let Some(messages) = response["result"]["messages"].as_array()
        {
            return messages
                .iter()
                .filter_map(|m| m["message"].as_str().map(String::from))
                .collect();
        }

        Vec::new()
    }

    /// Check if a message containing the given substring exists in the channel.
    pub fn channel_contains(&self, substring: &str) -> bool {
        let messages = self.read_channel_messages();
        messages.iter().any(|m| m.contains(substring))
    }

    /// Wait for a message containing the substring to appear in the channel.
    ///
    /// Returns true if the message appears within the timeout.
    pub fn wait_for_channel_message(&self, substring: &str, timeout_ms: u64) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < timeout_ms as u128 {
            if self.channel_contains(substring) {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// Create a task JSON file in the test's task directory.
    ///
    /// Creates a task with the given ID, subject, status, and optional owner.
    pub fn create_task(&self, id: &str, subject: &str, status: &str, owner: Option<&str>) {
        let _ = fs::create_dir_all(&self.tasks_dir);
        let task_json = serde_json::json!({
            "id": id,
            "subject": subject,
            "status": status,
            "owner": owner,
        });
        let task_path = self.tasks_dir.join(format!("{}.json", id));
        let _ = fs::write(task_path, serde_json::to_string_pretty(&task_json).unwrap());
    }

    /// Read daemon-state.json.
    pub fn read_daemon_state(&self) -> Option<serde_json::Value> {
        let path = self.project_dir.join("daemon-state.json");
        if !path.exists() {
            return None;
        }
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Send a webhook payload (convenience method).
    ///
    /// Panics if webhook is not enabled.
    pub fn send_webhook(&self, event_type: &str, payload: &str) -> Result<u16, String> {
        let client = WebhookTestClient::new(self).expect("Webhook not enabled");
        client.send_webhook(event_type, payload)
    }

    /// Send a webhook payload with optional signature validation (convenience method).
    ///
    /// Panics if webhook is not enabled.
    pub fn send_webhook_with_signature(
        &self,
        event_type: &str,
        payload: &str,
        valid_signature: bool,
    ) -> Result<u16, String> {
        let client = WebhookTestClient::new(self).expect("Webhook not enabled");
        client.send_webhook_with_signature(event_type, payload, valid_signature)
    }

    /// Get the coworkers directory for this test repo.
    pub fn coworkers_dir(&self) -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".midtown")
            .join("projects")
            .join(&self.repo_name)
            .join("coworkers")
    }

    /// Check if a worktree exists for a given coworker.
    pub fn worktree_exists(&self, coworker: &str) -> bool {
        self.coworkers_dir().join(coworker).exists()
    }
}

impl Drop for DaemonTestHarness {
    fn drop(&mut self) {
        // Stop daemon gracefully
        self.stop_daemon();

        // Clean up socket file and its parent directory
        let _ = fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir(parent);
        }
        let _ = fs::remove_dir_all(&self.state_dir);

        // Clean up project directory
        let _ = fs::remove_dir_all(&self.project_dir);

        // Clean up worktrees directory (now nested under projects/)
        let worktrees_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".midtown")
            .join("projects")
            .join(&self.repo_name)
            .join("worktrees");
        let _ = fs::remove_dir_all(&worktrees_dir);

        // Clean up task directory
        let _ = fs::remove_dir_all(&self.tasks_dir);

        // Clean up temp directory (the fake git repo)
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

// ── Webhook Test Client ─────────────────────────────────────────────

/// Client for sending GitHub webhook events to the daemon.
pub struct WebhookTestClient<'a> {
    harness: &'a DaemonTestHarness,
}

impl<'a> WebhookTestClient<'a> {
    /// Create a new webhook test client using the given harness.
    ///
    /// The harness must have webhook enabled.
    pub fn new(harness: &'a DaemonTestHarness) -> Option<Self> {
        harness.webhook_port?;
        Some(Self { harness })
    }

    /// Send a webhook payload with a valid signature.
    pub fn send_webhook(&self, event_type: &str, payload: &str) -> Result<u16, String> {
        self.send_webhook_with_signature(event_type, payload, true)
    }

    /// Send a webhook payload with optional valid signature.
    pub fn send_webhook_with_signature(
        &self,
        event_type: &str,
        payload: &str,
        valid_signature: bool,
    ) -> Result<u16, String> {
        let webhook_port = self.harness.webhook_port.ok_or("Webhook not enabled")?;
        let webhook_secret = self
            .harness
            .webhook_secret
            .as_ref()
            .ok_or("Webhook secret not set")?;

        let signature = if valid_signature {
            generate_signature(webhook_secret, payload.as_bytes())
        } else {
            "sha256=invalid".to_string()
        };

        let client = reqwest::blocking::Client::new();
        let url = format!("http://localhost:{}/webhook", webhook_port);

        let response = client
            .post(&url)
            .header("X-GitHub-Event", event_type)
            .header("X-Hub-Signature-256", signature)
            .header("Content-Type", "application/json")
            .body(payload.to_string())
            .send()
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        Ok(response.status().as_u16())
    }
}

/// Generate HMAC-SHA256 signature for webhook payload.
pub fn generate_signature(secret: &str, payload: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC key");
    mac.update(payload);
    let result = mac.finalize();
    format!("sha256={}", hex::encode(result.into_bytes()))
}
