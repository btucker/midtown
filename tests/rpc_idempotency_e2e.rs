//! End-to-end tests for RPC idempotency.
//!
//! These tests verify that retried RPC requests (due to timeouts) don't create
//! duplicate tasks or other unintended side effects. Addresses issue #1031.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Counter for unique test names across tests.
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique test repo name to avoid conflicts.
fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("rpc-idempotency-test-{}-{}", std::process::id(), counter)
}

/// Test fixture for RPC idempotency tests.
struct RpcFixture {
    temp_dir: PathBuf,
    project_dir: PathBuf,
    repo_name: String,
    socket_path: PathBuf,
    daemon_process: Option<Child>,
}

impl RpcFixture {
    fn new() -> Self {
        let repo_name = test_repo_name();

        // Create temp dir for fake git repo
        let temp_dir = std::env::temp_dir().join(&repo_name);
        fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        // Initialize fake git repo
        Command::new("git")
            .args(["init"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to init git repo");

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to configure git user name");

        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to configure git user email");

        // Create initial commit
        fs::write(temp_dir.join("README.md"), "test").expect("Failed to write README");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to git add");

        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to git commit");

        // Set git remote so daemon can infer the repo name correctly
        Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                &format!("git@github.com:test/{}.git", repo_name),
            ])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to set git remote");

        // Get project dir
        let project_dir = dirs::home_dir()
            .expect("Failed to get home dir")
            .join(".midtown")
            .join("projects")
            .join(&repo_name);

        // Get socket path
        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .expect("Failed to get home dir")
                    .join(".local")
                    .join("state")
            });
        let socket_path = state_dir
            .join("midtown")
            .join(&repo_name)
            .join("daemon.sock");

        Self {
            temp_dir,
            project_dir,
            repo_name,
            socket_path,
            daemon_process: None,
        }
    }

    /// Start the daemon process.
    fn start_daemon(&mut self) {
        // Build the daemon binary
        let status = Command::new("cargo")
            .args(["build", "--bin", "midtown"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to build midtown");
        assert!(status.success(), "Failed to build midtown");

        let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("midtown");

        // Remove stale socket if present
        let _ = fs::remove_file(&self.socket_path);

        // Start daemon with webhook disabled for testing
        let daemon = Command::new(&binary_path)
            .args(["daemon", "--workdir", self.temp_dir.to_str().unwrap()])
            .current_dir(&self.temp_dir)
            .env("MIDTOWN_WEBHOOK_PORT", "0")
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .env("RUST_LOG", "warn") // Reduce noise
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("Failed to start daemon");

        self.daemon_process = Some(daemon);

        // Wait for socket to exist and be connectable (up to 60 seconds)
        for _ in 0..300 {
            thread::sleep(Duration::from_millis(200));
            if self.socket_path.exists() && UnixStream::connect(&self.socket_path).is_ok() {
                return;
            }
        }
        panic!("Daemon socket did not become available within 60 seconds");
    }

    /// Send a JSON-RPC request and return the response.
    fn rpc_call(
        &self,
        method: &str,
        params: serde_json::Value,
        request_id: serde_json::Value,
    ) -> serde_json::Value {
        let mut stream =
            UnixStream::connect(&self.socket_path).expect("Failed to connect to daemon");

        // Set a reasonable timeout
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("Failed to set read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("Failed to set write timeout");

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": request_id
        });

        writeln!(stream, "{}", request).expect("Failed to write request");

        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();
        reader
            .read_line(&mut response_line)
            .expect("Failed to read response");

        serde_json::from_str(&response_line).expect("Failed to parse response")
    }

    /// Get all tasks from the shared task list.
    fn get_tasks(&self) -> Vec<TaskInfo> {
        let tasks_dir = dirs::home_dir()
            .expect("Failed to get home dir")
            .join(".claude")
            .join("tasks")
            .join(&self.repo_name);

        if !tasks_dir.exists() {
            return vec![];
        }

        let mut tasks = vec![];
        for entry in fs::read_dir(&tasks_dir).expect("Failed to read tasks dir") {
            let entry = entry.expect("Failed to read entry");
            if let Some(name) = entry.file_name().to_str()
                && name.ends_with(".md")
                && !name.starts_with('.')
            {
                let content = fs::read_to_string(entry.path()).expect("Failed to read task");
                if let Some(task_info) = parse_task(&content) {
                    tasks.push(task_info);
                }
            }
        }
        tasks.sort_by_key(|t| t.id);
        tasks
    }
}

impl Drop for RpcFixture {
    fn drop(&mut self) {
        // Kill daemon
        if let Some(mut daemon) = self.daemon_process.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }

        // Clean up temp dir
        let _ = fs::remove_dir_all(&self.temp_dir);

        // Clean up project dir
        let _ = fs::remove_dir_all(&self.project_dir);

        // Clean up socket dir
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }

        // Clean up tasks dir
        let tasks_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("tasks")
            .join(&self.repo_name);
        let _ = fs::remove_dir_all(&tasks_dir);
    }
}

#[derive(Debug, Clone)]
struct TaskInfo {
    id: u32,
    subject: String,
}

/// Parse a task file and extract basic info.
fn parse_task(content: &str) -> Option<TaskInfo> {
    let mut id = None;
    let mut subject = None;

    for line in content.lines() {
        if line.starts_with("id: ") {
            id = line.strip_prefix("id: ")?.parse().ok();
        } else if line.starts_with("subject: ") {
            subject = Some(line.strip_prefix("subject: ")?.to_string());
        }
    }

    Some(TaskInfo {
        id: id?,
        subject: subject?,
    })
}

#[test]
#[ignore] // E2E test - requires daemon
fn test_task_create_idempotency() {
    let mut fixture = RpcFixture::new();
    fixture.start_daemon();

    let subject = "Test idempotent task creation";
    let description = "This is a test task for idempotency verification";

    // Use the same request ID for all three calls to simulate retries
    let request_id = serde_json::json!("test-request-123");

    // Send the same task.create request 3 times rapidly with the SAME request ID
    // This simulates the bug where a timeout causes the client to retry
    let response1 = fixture.rpc_call(
        "task.create",
        serde_json::json!({
            "subject": subject,
            "description": description
        }),
        request_id.clone(),
    );

    let response2 = fixture.rpc_call(
        "task.create",
        serde_json::json!({
            "subject": subject,
            "description": description
        }),
        request_id.clone(),
    );

    let response3 = fixture.rpc_call(
        "task.create",
        serde_json::json!({
            "subject": subject,
            "description": description
        }),
        request_id,
    );

    // All responses should succeed
    assert_eq!(response1["jsonrpc"], "2.0");
    assert!(response1["result"].is_object());
    assert_eq!(response2["jsonrpc"], "2.0");
    assert!(response2["result"].is_object());
    assert_eq!(response3["jsonrpc"], "2.0");
    assert!(response3["result"].is_object());

    // Give daemon time to process
    thread::sleep(Duration::from_millis(500));

    // Check that only ONE task was created
    let tasks = fixture.get_tasks();
    let matching_tasks: Vec<_> = tasks.iter().filter(|t| t.subject == subject).collect();

    assert_eq!(
        matching_tasks.len(),
        1,
        "Expected exactly 1 task, but found {}: {:?}",
        matching_tasks.len(),
        matching_tasks
    );
}

#[test]
#[ignore] // E2E test - requires daemon
fn test_task_create_different_subjects_not_deduplicated() {
    let mut fixture = RpcFixture::new();
    fixture.start_daemon();

    // Create three tasks with different subjects using different request IDs
    fixture.rpc_call(
        "task.create",
        serde_json::json!({
            "subject": "Task A",
            "description": "Description A"
        }),
        serde_json::json!("request-A"),
    );

    fixture.rpc_call(
        "task.create",
        serde_json::json!({
            "subject": "Task B",
            "description": "Description B"
        }),
        serde_json::json!("request-B"),
    );

    fixture.rpc_call(
        "task.create",
        serde_json::json!({
            "subject": "Task C",
            "description": "Description C"
        }),
        serde_json::json!("request-C"),
    );

    thread::sleep(Duration::from_millis(500));

    let tasks = fixture.get_tasks();
    assert_eq!(
        tasks.len(),
        3,
        "Expected 3 different tasks, found {}",
        tasks.len()
    );
}
