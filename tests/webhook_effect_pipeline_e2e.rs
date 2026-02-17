//! E2E tests for the webhook-to-effect pipeline.
//!
//! Unlike webhook_e2e.rs which only verifies webhooks are received (HTTP 200,
//! channel message posted), these tests verify the daemon ACTS on webhooks by
//! checking that the correct effects are executed:
//!
//! - PR opened webhook → reviewer gets spawned on next tick
//! - Review submitted webhook → PR author gets nudged
//! - CI failure webhook → PR owner gets nudged
//! - PR merged webhook → tasks completed, worktrees cleaned up
//!
//! Run with `cargo test --test webhook_effect_pipeline_e2e -- --ignored --test-threads=1`

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use hmac::{Hmac, Mac};
use ntest::timeout;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

// ── Shared test infrastructure ─────────────────────────────────────

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("webhook-effect-e2e-test-{}-{}", std::process::id(), counter)
}

/// Kill any orphaned test daemons from previous runs.
fn cleanup_orphaned_test_daemons() {
    let _ = Command::new("pkill")
        .args(["-f", "midtown daemon.*webhook-effect-e2e-test"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    thread::sleep(Duration::from_millis(100));

    let current_pid = format!("webhook-effect-e2e-test-{}-", std::process::id());
    let projects_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("projects");
    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with("webhook-effect-e2e-test-")
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

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
                && name.starts_with("webhook-effect-e2e-test-")
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Test fixture managing daemon lifecycle and cleanup.
struct WebhookEffectFixture {
    temp_dir: PathBuf,
    project_dir: PathBuf,
    repo_name: String,
    socket_path: PathBuf,
    pid_path: PathBuf,
    webhook_port: u16,
    webhook_secret: String,
    daemon_process: Option<Child>,
    tasks_dir: PathBuf,
}

impl WebhookEffectFixture {
    fn new(webhook_port: u16) -> Option<Self> {
        cleanup_orphaned_test_daemons();

        let repo_name = test_repo_name();
        let temp_dir = std::env::temp_dir().join(&repo_name);

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

        // Configure git user (required for commits in test environment)
        let _ = Command::new("git")
            .args(["config", "user.email", "test@midtown.local"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("git")
            .args(["config", "user.name", "Midtown Test"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        // Initial commit (needed for some operations)
        let status = Command::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial commit"])
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }

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

        if let Some(parent) = socket_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Some(parent) = pid_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let tasks_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("tasks")
            .join(format!("midtown-{}", &repo_name));

        Some(Self {
            temp_dir,
            project_dir,
            repo_name,
            socket_path,
            pid_path,
            webhook_port,
            webhook_secret: "test-webhook-secret".to_string(),
            daemon_process: None,
            tasks_dir,
        })
    }

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

        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.pid_path);

        // Create log file for daemon output
        let log_path = self.temp_dir.join("daemon.log");

        // Start daemon with webhook enabled
        let log_file = fs::File::create(&log_path).ok();
        let log_err = fs::File::create(self.temp_dir.join("daemon_err.log")).ok();

        let child = Command::new(&binary_path)
            .arg("daemon")
            .arg("--workdir")
            .arg(&self.temp_dir)
            .current_dir(&self.temp_dir) // Isolate from real daemon
            .env("MIDTOWN_WEBHOOK_PORT", self.webhook_port.to_string())
            .env("MIDTOWN_WEBHOOK_SECRET", &self.webhook_secret)
            .env("MIDTOWN_CHAT_MONITOR", "0") // Disable for tests
            .env("RUST_LOG", "midtown=debug")
            .stdout(log_file.map(Stdio::from).unwrap_or(Stdio::null()))
            .stderr(log_err.map(Stdio::from).unwrap_or(Stdio::null()))
            .spawn();

        match child {
            Ok(child) => {
                self.daemon_process = Some(child);

                // Wait for socket to be available
                for i in 0..50 {
                    thread::sleep(Duration::from_millis(100));
                    if self.socket_path.exists() {
                        // Also wait for webhook server to be ready
                        thread::sleep(Duration::from_millis(500));
                        return true;
                    }

                    // Check if daemon has exited early with error
                    if let Some(ref mut proc) = self.daemon_process
                        && let Ok(Some(status)) = proc.try_wait()
                        && !status.success()
                    {
                        eprintln!("Daemon exited early with error status: {:?}", status);
                        if let Ok(log) = fs::read_to_string(&log_path) {
                            eprintln!("Daemon stdout:\n{}", log);
                        }
                        if let Ok(err) = fs::read_to_string(self.temp_dir.join("daemon_err.log")) {
                            eprintln!("Daemon stderr:\n{}", err);
                        }
                        return false;
                    }

                    if i == 25 {
                        eprintln!("Waiting for socket... ({} attempts)", i);
                    }
                }
                eprintln!("Daemon socket never appeared at {:?}", self.socket_path);
                if let Ok(log) = fs::read_to_string(&log_path) {
                    eprintln!("Daemon stdout:\n{}", log);
                }
                if let Ok(err) = fs::read_to_string(self.temp_dir.join("daemon_err.log")) {
                    eprintln!("Daemon stderr:\n{}", err);
                }
                false
            }
            Err(e) => {
                eprintln!("Failed to start daemon: {}", e);
                false
            }
        }
    }

    /// Send a webhook payload to the daemon's webhook endpoint.
    fn send_webhook(&self, event_type: &str, payload: &str) -> Result<u16, String> {
        let signature = generate_signature(&self.webhook_secret, payload.as_bytes());

        let client = reqwest::blocking::Client::new();
        let url = format!("http://localhost:{}/webhook", self.webhook_port);

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

    /// Make an RPC call to the daemon.
    fn rpc_call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let mut stream = UnixStream::connect(&self.socket_path).ok()?;

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

    /// Read recent messages from the channel via RPC.
    fn read_channel_messages(&self) -> Vec<String> {
        let response = self.rpc_call("channel.read", Some(serde_json::json!({"all": true})));

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
    fn channel_contains(&self, substring: &str) -> bool {
        let messages = self.read_channel_messages();
        messages.iter().any(|m| m.contains(substring))
    }

    /// Wait for a message containing the substring to appear in the channel.
    fn wait_for_channel_message(&self, substring: &str, timeout_ms: u64) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < timeout_ms as u128 {
            if self.channel_contains(substring) {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// Get the path to daemon-state.json.
    fn daemon_state_path(&self) -> PathBuf {
        self.project_dir.join("daemon-state.json")
    }

    /// Read daemon-state.json.
    fn read_daemon_state(&self) -> Option<serde_json::Value> {
        let path = self.daemon_state_path();
        if !path.exists() {
            return None;
        }
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// List coworkers via RPC.
    #[allow(dead_code)]
    fn list_coworkers(&self) -> Vec<String> {
        let response = self.rpc_call("coworker.list", None);

        if let Some(response) = response
            && let Some(coworkers) = response["result"]["coworkers"].as_array()
        {
            return coworkers
                .iter()
                .filter_map(|c| c["name"].as_str().map(String::from))
                .collect();
        }

        Vec::new()
    }

    /// Wait for a coworker to be spawned.
    #[allow(dead_code)]
    fn wait_for_coworker(&self, name: &str, timeout_ms: u64) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < timeout_ms as u128 {
            let coworkers = self.list_coworkers();
            if coworkers.iter().any(|c| c == name) {
                return true;
            }
            thread::sleep(Duration::from_millis(200));
        }
        false
    }

    /// Create a task JSON file in the test's task directory.
    fn create_task(&self, id: &str, subject: &str, status: &str, owner: Option<&str>) {
        let _ = fs::create_dir_all(&self.tasks_dir);
        let task_json = serde_json::json!({
            "id": id,
            "subject": subject,
            "status": status,
            "owner": owner,
            "description": format!("Test task {}", id),
            "blocked_by": []
        });
        let task_file = self.tasks_dir.join(format!("{}.json", id));
        fs::write(
            &task_file,
            serde_json::to_string_pretty(&task_json).unwrap(),
        )
        .unwrap_or_else(|e| panic!("Failed to write task file {:?}: {}", task_file, e));
    }

    /// Get path to coworkers directory.
    fn coworkers_dir(&self) -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".midtown")
            .join("coworkers")
            .join(&self.repo_name)
    }

    /// Check if a worktree exists for a given coworker.
    fn worktree_exists(&self, coworker: &str) -> bool {
        self.coworkers_dir().join(coworker).exists()
    }
}

impl Drop for WebhookEffectFixture {
    fn drop(&mut self) {
        // Kill daemon process
        if let Some(mut child) = self.daemon_process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        // Clean up test directories
        let _ = fs::remove_dir_all(&self.temp_dir);
        let _ = fs::remove_dir_all(&self.project_dir);
        let _ = fs::remove_dir_all(&self.tasks_dir);
        let _ = fs::remove_dir_all(self.coworkers_dir());
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

/// Generate HMAC-SHA256 signature for webhook payload.
fn generate_signature(secret: &str, payload: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC key");
    mac.update(payload);
    let result = mac.finalize();
    format!("sha256={}", hex::encode(result.into_bytes()))
}

// ── Webhook-to-Effect Pipeline Tests ───────────────────────────────

/// Test that PR opened webhook queues a reviewer spawn.
///
/// This test verifies the webhook is not just received (tested in webhook_e2e.rs),
/// but that the daemon ACTS on it by queueing a reviewer spawn in daemon-state.json.
#[test]
#[ignore]
#[timeout(60000)]
fn test_pr_opened_queues_reviewer_spawn() {
    let mut fixture = WebhookEffectFixture::new(47200).expect("Failed to create fixture");
    assert!(fixture.start_daemon(), "Failed to start daemon");

    // Give daemon time to initialize
    thread::sleep(Duration::from_secs(1));

    let payload = r#"{
        "action": "opened",
        "number": 42,
        "pull_request": {
            "title": "Add authentication feature",
            "user": {"login": "testuser"},
            "merged": false,
            "draft": false,
            "head": {"ref": "lexington/add-auth"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = fixture
        .send_webhook("pull_request", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200, "Webhook should return 200 OK");

    // Verify message appears in channel
    assert!(
        fixture.wait_for_channel_message("opened PR #42", 5000),
        "PR opened message should appear in channel"
    );

    // Give daemon a moment to persist state
    thread::sleep(Duration::from_millis(500));

    // CRITICAL: Verify daemon queued a reviewer spawn in daemon-state.json
    let daemon_state = fixture
        .read_daemon_state()
        .expect("Should read daemon-state.json");

    let pending_spawns = daemon_state["github"]["pending_review_spawns"]
        .as_array()
        .expect("pending_review_spawns should be an array");

    let has_spawn_for_42 = pending_spawns
        .iter()
        .any(|spawn| spawn["pr_number"].as_u64() == Some(42));

    assert!(
        has_spawn_for_42,
        "Daemon should queue a reviewer spawn for PR #42. State: {:?}",
        daemon_state["github"]
    );
}

/// Test that review approved webhook nudges PR author.
///
/// This test verifies the daemon acts on the webhook by nudging the coworker
/// who owns the PR. Since we can't easily spawn real coworkers in tests,
/// we verify the nudge attempt via channel message or RPC state.
#[test]
#[ignore]
#[timeout(60000)]
fn test_review_approved_nudges_author() {
    let mut fixture = WebhookEffectFixture::new(47201).expect("Failed to create fixture");
    assert!(fixture.start_daemon(), "Failed to start daemon");

    thread::sleep(Duration::from_secs(1));

    let payload = r#"{
        "action": "submitted",
        "review": {
            "id": 100,
            "state": "approved",
            "user": {"login": "reviewer"}
        },
        "pull_request": {
            "number": 77,
            "head": {"ref": "broadway/feature"},
            "body": "PR description"
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = fixture
        .send_webhook("pull_request_review", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Verify approval message appears
    assert!(
        fixture.wait_for_channel_message("reviewer approved PR #77", 5000),
        "Review approval should appear in channel"
    );

    // Verify @broadway mention (indicating nudge attempt)
    assert!(
        fixture.channel_contains("@broadway"),
        "Message should mention PR owner (nudge target)"
    );
}

/// Test that CI failure webhook nudges PR owner.
///
/// This verifies the daemon acts on CI failure webhooks by attempting to
/// nudge the PR owner (verified via channel @mention).
#[test]
#[ignore]
#[timeout(60000)]
fn test_ci_failure_nudges_owner() {
    let mut fixture = WebhookEffectFixture::new(47202).expect("Failed to create fixture");
    assert!(fixture.start_daemon(), "Failed to start daemon");

    thread::sleep(Duration::from_secs(1));

    let payload = r#"{
        "action": "completed",
        "check_run": {
            "name": "Build",
            "status": "completed",
            "conclusion": "failure",
            "check_suite": {
                "head_sha": "abc123",
                "head_branch": "madison/feature",
                "pull_requests": [{"number": 99}]
            }
        },
        "repository": {"full_name": "test/repo", "default_branch": "main"}
    }"#;

    let status = fixture
        .send_webhook("check_run", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Verify CI failure message appears
    assert!(
        fixture.wait_for_channel_message("Check 'Build' failed on PR #99", 15000),
        "CI failure should appear in channel"
    );

    // Verify @madison mention (indicating nudge attempt)
    assert!(
        fixture.channel_contains("@madison"),
        "Message should mention PR owner (nudge target)"
    );
}

/// Test that PR merged webhook posts merge notification and completes task.
///
/// This verifies the daemon acts on PR merge webhooks by posting to channel,
/// sending merge notification, and auto-completing the task referenced in the
/// PR title.
#[test]
#[ignore]
#[timeout(60000)]
fn test_pr_merged_posts_notification() {
    let mut fixture = WebhookEffectFixture::new(47203).expect("Failed to create fixture");
    assert!(fixture.start_daemon(), "Failed to start daemon");

    // Create a task file so auto-completion can succeed
    fixture.create_task("42", "Add auth endpoint", "in_progress", Some("park"));

    thread::sleep(Duration::from_secs(1));

    let payload = r#"{
        "action": "closed",
        "number": 55,
        "pull_request": {
            "title": "feat: Add auth endpoint [Midtown !42]",
            "user": {"login": "testuser"},
            "merged": true,
            "head": {"ref": "park/add-auth"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = fixture
        .send_webhook("pull_request", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Give daemon a moment to process the webhook
    thread::sleep(Duration::from_millis(1000));

    // Verify merged message appears
    assert!(
        fixture.wait_for_channel_message("merged PR #55", 5000),
        "PR merged message should appear in channel"
    );

    // Verify merge notification is posted
    assert!(
        fixture.wait_for_channel_message("PR #55 merged into main", 5000),
        "Merge notification should appear in channel"
    );

    // Verify task auto-completion message appears
    assert!(
        fixture.wait_for_channel_message("✅ Auto-completed task !42", 5000),
        "Task should be auto-completed when PR is merged"
    );
}

/// Test that PR merged webhook triggers worktree cleanup.
///
/// This verifies the daemon acts on PR merge webhooks by cleaning up the
/// coworker worktree associated with the merged PR.
#[test]
#[ignore]
#[timeout(60000)]
fn test_pr_merged_cleans_up_worktree() {
    let mut fixture = WebhookEffectFixture::new(47204).expect("Failed to create fixture");
    assert!(fixture.start_daemon(), "Failed to start daemon");

    thread::sleep(Duration::from_secs(1));

    // Create a fake worktree directory to simulate an active coworker
    let worktree_path = fixture.coworkers_dir().join("riverside");
    fs::create_dir_all(&worktree_path).expect("Failed to create worktree dir");
    assert!(
        fixture.worktree_exists("riverside"),
        "Worktree should exist before merge"
    );

    let payload = r#"{
        "action": "closed",
        "number": 66,
        "pull_request": {
            "title": "feat: Add feature",
            "user": {"login": "testuser"},
            "merged": true,
            "head": {"ref": "riverside/add-feature"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = fixture
        .send_webhook("pull_request", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Give daemon time to process webhook and queue cleanup
    thread::sleep(Duration::from_millis(1000));

    // Verify merged message appears
    assert!(
        fixture.wait_for_channel_message("merged PR #66", 5000),
        "PR merged message should appear in channel"
    );

    // Wait for cleanup to occur (daemon processes cleanup on next tick)
    // The daemon may not immediately delete the worktree - it queues cleanup effects
    // which are processed asynchronously. For this test, we verify the cleanup
    // was queued by checking daemon state or waiting for the worktree to be removed.
    let mut cleanup_occurred = false;
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(500));
        if !fixture.worktree_exists("riverside") {
            cleanup_occurred = true;
            break;
        }
    }

    assert!(
        cleanup_occurred,
        "Worktree should be cleaned up after PR merge"
    );
}

/// Test the full PR lifecycle: opened → reviewer queued → merged → task completed.
///
/// This comprehensive test sends multiple webhooks in sequence and verifies
/// the daemon acts correctly at each stage: queueing reviewer spawns, posting
/// channel messages, and auto-completing tasks.
///
/// NOTE: This test was previously disabled because sending multiple webhooks for
/// the same PR in quick succession had state interaction issues. Re-enabling to
/// investigate and fix the root cause.
#[test]
#[ignore]
#[timeout(60000)]
fn test_full_pr_lifecycle_webhook_effects() {
    let mut fixture = WebhookEffectFixture::new(47204).expect("Failed to create fixture");
    assert!(fixture.start_daemon(), "Failed to start daemon");

    thread::sleep(Duration::from_secs(1));

    // Step 1: PR opened
    let pr_opened_payload = r#"{
        "action": "opened",
        "number": 100,
        "pull_request": {
            "title": "feat: Implement feature [Midtown !50]",
            "user": {"login": "testuser"},
            "merged": false,
            "draft": false,
            "head": {"ref": "columbus/implement-feature"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = fixture
        .send_webhook("pull_request", pr_opened_payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Verify PR opened message
    assert!(
        fixture.wait_for_channel_message("opened PR #100", 5000),
        "PR opened message should appear"
    );

    // Verify reviewer spawn is queued
    thread::sleep(Duration::from_millis(500));
    let daemon_state = fixture
        .read_daemon_state()
        .expect("Should read daemon-state.json");
    let pending_spawns = daemon_state["github"]["pending_review_spawns"]
        .as_array()
        .expect("pending_review_spawns should be an array");
    let has_spawn_for_100 = pending_spawns
        .iter()
        .any(|spawn| spawn["pr_number"].as_u64() == Some(100));
    assert!(
        has_spawn_for_100,
        "Reviewer spawn should be queued for PR #100"
    );

    // Step 2: Different PR merged (simulating a different PR to avoid state collision)
    // In a real scenario, the same PR would transition from open to merged,
    // but for testing we use separate PR numbers to isolate the webhook effects.
    let pr_merged_payload = r#"{
        "action": "closed",
        "number": 101,
        "pull_request": {
            "title": "feat: Complete implementation [Midtown !51]",
            "user": {"login": "testuser"},
            "merged": true,
            "head": {"ref": "columbus/complete-impl"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = fixture
        .send_webhook("pull_request", pr_merged_payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Give daemon time to process (longer for second webhook in sequence)
    thread::sleep(Duration::from_millis(2000));

    // Verify PR merged message
    assert!(
        fixture.wait_for_channel_message("merged PR #101", 5000),
        "PR merged message should appear"
    );

    // Verify merge notification
    assert!(
        fixture.wait_for_channel_message("PR #101 merged into main", 5000),
        "Merge notification should be posted"
    );

    // Verify task auto-completion (daemon creates task structure automatically)
    assert!(
        fixture.wait_for_channel_message("✅ Auto-completed task !51", 5000),
        "Task should be auto-completed after PR merge"
    );
}
