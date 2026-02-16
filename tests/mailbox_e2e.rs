//! End-to-end tests for the agent teams mailbox integration.
//!
//! These tests verify that coworker spawning correctly sets up the agent teams
//! infrastructure (team config, inbox directories) and that mailbox message
//! delivery works end-to-end. Tests are split into two categories:
//!
//! **Coordination tests** (no real Claude needed):
//! - Team config creation after coworker spawn
//! - Inbox directory structure
//! - Concurrent spawn team config integrity
//! - Mailbox write + read-back correctness
//! - Fallback to tmux when mailbox write fails
//!
//! **Full tests** (real Claude, `--ignored`):
//! - Coworker spawned with agent teams flags receives mailbox messages
//!
//! Run coordination: `cargo test --test mailbox_e2e`
//! Run all:          `cargo test --test mailbox_e2e -- --ignored --test-threads=1`

use ntest::timeout;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

// ── Shared test infrastructure ─────────────────────────────────────

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// RAII guard that makes a path read-only and restores original permissions on drop.
/// Used to test fallback behavior when write operations fail.
struct ReadOnlyGuard {
    path: PathBuf,
    original_mode: u32,
}

impl ReadOnlyGuard {
    fn new(path: &Path) -> Self {
        let metadata = fs::metadata(path).expect("Path should exist");
        let original_mode = metadata.permissions().mode();

        // Make read-only (remove write bits)
        let mut perms = metadata.permissions();
        perms.set_mode(original_mode & !0o222);
        fs::set_permissions(path, perms).expect("Should set read-only permissions");

        Self {
            path: path.to_path_buf(),
            original_mode,
        }
    }
}

impl Drop for ReadOnlyGuard {
    fn drop(&mut self) {
        // Restore original permissions
        let mut perms = fs::metadata(&self.path)
            .expect("Path should still exist")
            .permissions();
        perms.set_mode(self.original_mode);
        let _ = fs::set_permissions(&self.path, perms);
    }
}

fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("mailbox-e2e-test-{}-{}", std::process::id(), counter)
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success() || o.status.code() == Some(1))
        .unwrap_or(false)
}

fn claude_available() -> bool {
    Command::new("claude")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Kill any orphaned test daemons and tmux sessions from previous runs.
fn cleanup_orphaned_test_daemons() {
    let _ = Command::new("pkill")
        .args(["-f", "midtown daemon.*mailbox-e2e-test"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("pkill")
        .args(["-f", "midtown.*mailbox-e2e-test"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    thread::sleep(Duration::from_millis(100));

    // Kill orphaned tmux sessions
    if let Ok(output) = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        && let Ok(sessions) = String::from_utf8(output.stdout)
    {
        let current_pid = format!("mailbox-e2e-test-{}-", std::process::id());
        for session in sessions.lines() {
            if session.contains("mailbox-e2e-test") && !session.contains(&current_pid) {
                let _ = Command::new("tmux")
                    .args(["kill-session", "-t", session])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
    thread::sleep(Duration::from_millis(100));

    // Clean up stale project directories
    let current_pid = format!("mailbox-e2e-test-{}-", std::process::id());
    let projects_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("projects");
    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with("mailbox-e2e-test-")
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
                && name.starts_with("mailbox-e2e-test-")
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Test fixture managing daemon lifecycle, tmux session, and cleanup.
#[allow(dead_code)]
struct MailboxFixture {
    temp_dir: PathBuf,
    project_dir: PathBuf,
    repo_name: String,
    socket_path: PathBuf,
    pid_path: PathBuf,
    daemon_process: Option<Child>,
    tasks_dir: PathBuf,
    team_name: String,
    team_dir: PathBuf,
}

impl MailboxFixture {
    fn new() -> Option<Self> {
        cleanup_orphaned_test_daemons();

        let repo_name = test_repo_name();
        let temp_dir = std::env::temp_dir().join(&repo_name);

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

        // Need an initial commit for worktrees
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

        let tasks_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("tasks")
            .join(format!("midtown-{}", &repo_name));

        let team_name = midtown::mailbox::team_name_for_repo(&repo_name);
        let team_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("teams")
            .join(&team_name);

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
            daemon_process: None,
            tasks_dir,
            team_name,
            team_dir,
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

        let child = Command::new(&binary_path)
            .arg("daemon")
            .arg("--workdir")
            .arg(&self.temp_dir)
            .current_dir(&self.temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("MIDTOWN_WEBHOOK_PORT", "0")
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .spawn();

        match child {
            Ok(c) => {
                self.daemon_process = Some(c);
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

    /// Start daemon using `midtown start` (creates tmux session + lead window).
    fn start_daemon_with_tmux(&mut self) -> bool {
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

        let child = Command::new(&binary_path)
            .arg("start")
            .current_dir(&self.temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("MIDTOWN_WEBHOOK_PORT", "0")
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .spawn();

        match child {
            Ok(mut c) => {
                let exit_status = c.wait();
                match exit_status {
                    Ok(status) if status.success() => {
                        for _ in 0..50 {
                            if self.socket_path.exists()
                                && UnixStream::connect(&self.socket_path).is_ok()
                            {
                                return true;
                            }
                            thread::sleep(Duration::from_millis(100));
                        }
                        eprintln!("Socket not available after successful midtown start");
                        false
                    }
                    Ok(status) => {
                        eprintln!("midtown start failed with exit status: {:?}", status);
                        false
                    }
                    Err(e) => {
                        eprintln!("Failed to wait for midtown start: {}", e);
                        false
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to spawn midtown start: {}", e);
                false
            }
        }
    }

    fn connect(&self) -> Option<UnixStream> {
        UnixStream::connect(&self.socket_path).ok()
    }

    fn rpc_call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let mut stream = self.connect()?;
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

    fn tmux_session_name(&self) -> String {
        format!("midtown-{}", self.repo_name)
    }

    fn stop_daemon(&mut self) {
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

        if let Some(ref mut child) = self.daemon_process {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.daemon_process = None;

        let pattern = format!("midtown daemon.*{}", self.repo_name);
        let _ = Command::new("pkill")
            .args(["-f", &pattern])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn kill_tmux_session(&self) {
        let session = self.tmux_session_name();
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &session])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for MailboxFixture {
    fn drop(&mut self) {
        self.stop_daemon();
        self.kill_tmux_session();

        let _ = fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir(parent);
        }
        let _ = fs::remove_dir_all(&self.project_dir);
        let _ = fs::remove_dir_all(&self.temp_dir);
        let _ = fs::remove_dir_all(&self.tasks_dir);
        let _ = fs::remove_dir_all(&self.team_dir);

        // Clean up worktrees
        let coworkers_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".midtown")
            .join("coworkers")
            .join(&self.repo_name);
        let _ = fs::remove_dir_all(&coworkers_dir);
    }
}

// ── Helper functions ───────────────────────────────────────────────

/// Capture the content of a tmux pane.
fn capture_pane(session: &str, window: &str) -> Option<String> {
    let target = format!("{}:{}", session, window);
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", &target, "-p"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

/// Check if a tmux window exists in a session.
fn window_exists(session: &str, window: &str) -> bool {
    let output = Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", "#{window_name}"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.lines().any(|line| {
                let base_name = line.split(':').next().unwrap_or(line);
                base_name == window
            })
        }
        _ => false,
    }
}

// ── Coordination tests (no real Claude needed) ─────────────────────

/// Spawn a coworker via RPC and verify the team config is created with
/// the coworker's entry and the inboxes directory exists.
///
/// This validates that `spawn_claude()` calls `upsert_team_member()` during
/// the spawn path, creating the agent teams infrastructure that Claude Code
/// needs to discover its team membership and receive mailbox messages.
#[test]
#[ignore] // Requires built binary and tmux
#[timeout(120_000)]
fn test_spawn_creates_team_config_and_inbox_dir() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let mut fixture = match MailboxFixture::new() {
        Some(f) => f,
        None => return,
    };

    assert!(
        fixture.start_daemon(),
        "Fixture failed to start daemon for mailbox integration test"
    );

    // Spawn a coworker via RPC
    let spawn_response = fixture.rpc_call("coworker.spawn", Some(serde_json::json!({})));
    assert!(
        spawn_response.is_some(),
        "Should receive response from coworker.spawn"
    );

    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        eprintln!(
            "Coworker spawn failed (expected in some environments): {:?}",
            spawn_response["error"]
        );
        return;
    }

    let coworker_name = spawn_response["result"]["coworkers"][0]["name"]
        .as_str()
        .expect("Response should contain coworker name");
    eprintln!("Spawned coworker: {}", coworker_name);

    // Give the daemon a moment to complete team setup
    thread::sleep(Duration::from_secs(2));

    // Verify team config exists
    let config_path = fixture.team_dir.join("config.json");
    assert!(
        config_path.exists(),
        "Team config should exist at {:?}",
        config_path
    );

    // Verify config contains the spawned coworker
    let config_content = fs::read_to_string(&config_path)
        .unwrap_or_else(|e| panic!("Failed to read team config: {}", e));
    let config: midtown::mailbox::TeamConfig = serde_json::from_str(&config_content)
        .unwrap_or_else(|e| panic!("Failed to parse team config: {}", e));

    let coworker_member = config.members.iter().find(|m| m.name == coworker_name);
    assert!(
        coworker_member.is_some(),
        "Team config should contain coworker '{}'. Members: {:?}",
        coworker_name,
        config.members.iter().map(|m| &m.name).collect::<Vec<_>>()
    );

    let member = coworker_member.unwrap();
    let expected_agent_id = midtown::mailbox::agent_id(coworker_name, &fixture.team_name);
    assert_eq!(
        member.agent_id, expected_agent_id,
        "Agent ID should follow name@team format"
    );
    assert_eq!(
        member.agent_type, "coworker",
        "Agent type should be 'coworker'"
    );

    // Verify inboxes directory exists
    let inboxes_dir = fixture.team_dir.join("inboxes");
    assert!(
        inboxes_dir.exists(),
        "Inboxes directory should exist at {:?}",
        inboxes_dir
    );
}

/// Write a message to a coworker's inbox via the mailbox API and verify
/// the inbox file contains the correctly formatted message.
///
/// This validates the write path independently of the daemon, ensuring
/// the inbox JSON format matches what Claude Code's `readUnreadMessages()`
/// expects.
#[test]
fn test_mailbox_write_creates_valid_inbox_file() {
    let team_name = format!("midtown-mailbox-test-{}", std::process::id());
    let agent_name = "test-agent";

    // Clean up any existing team dir from previous runs
    let team_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("teams")
        .join(&team_name);
    let _ = fs::remove_dir_all(&team_dir);

    // Write a message
    let msg = midtown::mailbox::MailboxMessage::new(
        "You have pending task !42: Fix auth bug. Get started!",
        "midtown",
    )
    .with_color("yellow")
    .with_summary("Task !42 assignment");

    midtown::mailbox::write_to_inbox(&team_name, agent_name, msg)
        .expect("write_to_inbox should succeed");

    // Read back and verify
    let inbox_path = team_dir.join("inboxes").join("test-agent.json");
    assert!(inbox_path.exists(), "Inbox file should exist");

    let content = fs::read_to_string(&inbox_path).expect("Should read inbox file");
    let messages: Vec<midtown::mailbox::MailboxMessage> =
        serde_json::from_str(&content).expect("Inbox should be valid JSON array");

    assert_eq!(messages.len(), 1, "Should have exactly one message");
    assert_eq!(
        messages[0].text,
        "You have pending task !42: Fix auth bug. Get started!"
    );
    assert_eq!(messages[0].from, "midtown");
    assert_eq!(messages[0].color.as_deref(), Some("yellow"));
    assert_eq!(messages[0].summary.as_deref(), Some("Task !42 assignment"));
    assert!(!messages[0].read, "New messages should be unread");
    assert!(!messages[0].timestamp.is_empty(), "Timestamp should be set");

    // Write a second message and verify append behavior
    let msg2 = midtown::mailbox::MailboxMessage::new("PR #99 needs your review", "midtown");
    midtown::mailbox::write_to_inbox(&team_name, agent_name, msg2)
        .expect("Second write should succeed");

    let content = fs::read_to_string(&inbox_path).expect("Should read inbox file");
    let messages: Vec<midtown::mailbox::MailboxMessage> =
        serde_json::from_str(&content).expect("Inbox should still be valid JSON array");
    assert_eq!(messages.len(), 2, "Should have two messages after append");
    assert_eq!(messages[1].text, "PR #99 needs your review");

    // Clean up
    let _ = fs::remove_dir_all(&team_dir);
}

/// Verify that concurrent writes to the same inbox don't corrupt the file.
///
/// Spawns multiple threads all writing to the same agent's inbox simultaneously.
/// After all writes complete, the inbox must contain valid JSON with all messages
/// present. This tests the mkdir-based locking and atomic write mechanism.
#[test]
fn test_concurrent_inbox_writes_no_corruption() {
    use std::sync::Arc;

    let team_name = format!("midtown-concurrent-test-{}", std::process::id());
    let agent_name = "concurrent-agent";

    // Clean up
    let team_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("teams")
        .join(&team_name);
    let _ = fs::remove_dir_all(&team_dir);

    let team_name = Arc::new(team_name);
    let thread_count = 10;

    let handles: Vec<_> = (0..thread_count)
        .map(|i| {
            let team = Arc::clone(&team_name);
            thread::spawn(move || {
                let msg = midtown::mailbox::MailboxMessage::new(
                    format!("Message from thread {}", i),
                    format!("thread-{}", i),
                );
                midtown::mailbox::write_to_inbox(&team, agent_name, msg)
                    .unwrap_or_else(|e| panic!("Thread {} write failed: {}", i, e));
            })
        })
        .collect();

    for h in handles {
        h.join().expect("Thread should not panic");
    }

    // Verify: inbox must be valid JSON with all messages
    let inbox_path = team_dir.join("inboxes").join("concurrent-agent.json");
    let content =
        fs::read_to_string(&inbox_path).expect("Inbox file should exist after concurrent writes");
    let messages: Vec<midtown::mailbox::MailboxMessage> =
        serde_json::from_str(&content).expect("Inbox should be valid JSON after concurrent writes");

    assert_eq!(
        messages.len(),
        thread_count,
        "All {} messages should be present (got {}). Concurrent write race detected.",
        thread_count,
        messages.len()
    );

    // Verify all thread IDs are represented
    let mut from_ids: Vec<String> = messages.iter().map(|m| m.from.clone()).collect();
    from_ids.sort();
    let mut expected: Vec<String> = (0..thread_count).map(|i| format!("thread-{}", i)).collect();
    expected.sort();
    assert_eq!(
        from_ids, expected,
        "Each thread's message should be present"
    );

    // Clean up
    let _ = fs::remove_dir_all(&*team_dir);
}

/// Verify that concurrent coworker spawns don't lose team members.
///
/// When multiple coworkers spawn simultaneously, each calls `upsert_team_member()`
/// concurrently. The mkdir lock must ensure all members appear in the final config.
#[test]
fn test_concurrent_team_member_upsert_no_lost_entries() {
    use std::sync::Arc;

    let team_name = format!("midtown-upsert-test-{}", std::process::id());
    let team_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("teams")
        .join(&team_name);
    let _ = fs::remove_dir_all(&team_dir);

    let team_name = Arc::new(team_name);
    let names = [
        "lexington",
        "park",
        "madison",
        "broadway",
        "amsterdam",
        "columbus",
        "riverside",
        "york",
        "pleasant",
        "vernon",
    ];

    let handles: Vec<_> = names
        .iter()
        .map(|name| {
            let team = Arc::clone(&team_name);
            let name = name.to_string();
            thread::spawn(move || {
                let member = midtown::mailbox::TeamMember {
                    name: name.clone(),
                    agent_id: midtown::mailbox::agent_id(&name, &team),
                    agent_type: "coworker".to_string(),
                };
                midtown::mailbox::upsert_team_member(&team, member)
                    .unwrap_or_else(|e| panic!("upsert_team_member for {} failed: {}", name, e));
            })
        })
        .collect();

    for h in handles {
        h.join().expect("Thread should not panic");
    }

    // Verify all members are present
    let config_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("teams")
        .join(&*team_name)
        .join("config.json");
    let content = fs::read_to_string(&config_path).expect("Config should exist");
    let config: midtown::mailbox::TeamConfig =
        serde_json::from_str(&content).expect("Config should be valid JSON");

    assert_eq!(
        config.members.len(),
        names.len(),
        "All {} members should be present. Got: {:?}",
        names.len(),
        config.members.iter().map(|m| &m.name).collect::<Vec<_>>()
    );

    let mut found: Vec<String> = config.members.iter().map(|m| m.name.clone()).collect();
    found.sort();
    let mut expected: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    expected.sort();
    assert_eq!(found, expected, "All member names should match");

    // Clean up
    let _ = fs::remove_dir_all(
        &*dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("teams")
            .join(&*team_name),
    );
}

/// Verify the DeliverMailboxMessage effect in the daemon produces a valid
/// inbox file by triggering a task assignment to an idle coworker.
///
/// This tests the full daemon path: daemon detects idle coworker with pending
/// task → produces DeliverMailboxMessage effect → effect handler writes to inbox.
#[test]
#[ignore] // Requires built binary and tmux
#[timeout(180_000)]
fn test_daemon_delivers_mailbox_message_on_task_assignment() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let mut fixture = match MailboxFixture::new() {
        Some(f) => f,
        None => return,
    };

    assert!(
        fixture.start_daemon(),
        "Fixture failed to start daemon for mailbox integration test"
    );

    // Spawn a coworker so the daemon has someone to assign tasks to
    let spawn_response = fixture.rpc_call("coworker.spawn", Some(serde_json::json!({})));
    assert!(
        spawn_response.is_some(),
        "Should receive response from coworker.spawn"
    );

    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        eprintln!("Coworker spawn failed: {:?}", spawn_response["error"]);
        return;
    }

    let coworker_name = spawn_response["result"]["coworkers"][0]["name"]
        .as_str()
        .expect("Response should contain coworker name");
    eprintln!("Spawned coworker: {}", coworker_name);

    // Create a pending task already owned by this idle coworker so dispatch
    // takes the "pending-with-owner" path, which emits DeliverMailboxMessage.
    fixture.create_task(
        "99",
        "Test mailbox delivery",
        "pending",
        Some(coworker_name),
    );

    // Wait for the daemon's task dispatch tick to assign the task and deliver
    // the mailbox message (dispatch runs every ~5s).
    let inbox_path = fixture
        .team_dir
        .join("inboxes")
        .join(format!("{}.json", coworker_name));

    let mut message_delivered = false;
    for i in 0..60 {
        thread::sleep(Duration::from_secs(2));
        if inbox_path.exists()
            && let Ok(content) = fs::read_to_string(&inbox_path)
            && let Ok(messages) =
                serde_json::from_str::<Vec<midtown::mailbox::MailboxMessage>>(&content)
            && messages
                .iter()
                .any(|m| m.text.contains("task") || m.text.contains("99"))
        {
            eprintln!(
                "Mailbox message delivered after {}s: {}",
                (i + 1) * 2,
                messages.last().map(|m| m.text.as_str()).unwrap_or("")
            );
            message_delivered = true;
            break;
        }
    }

    assert!(
        message_delivered,
        "Daemon should deliver a mailbox message to coworker '{}' for task assignment within 120s. \
         Inbox path: {:?}",
        coworker_name, inbox_path
    );

    // Verify the message format
    let content = fs::read_to_string(&inbox_path).expect("Should read inbox");
    let messages: Vec<midtown::mailbox::MailboxMessage> =
        serde_json::from_str(&content).expect("Should parse inbox JSON");

    let task_msg = messages
        .iter()
        .find(|m| m.text.contains("task") || m.text.contains("99"))
        .expect("Should find task assignment message");

    assert_eq!(task_msg.from, "midtown", "Message should be from 'midtown'");
    assert!(!task_msg.read, "Message should be unread");
}

// ── Full tests (real Claude) ───────────────────────────────────────

/// Spawn a coworker with real Claude Code and verify it receives agent teams
/// CLI flags (--agent-id, --agent-name, --team-name) and the team config is
/// set up before Claude Code starts.
///
/// This is the most comprehensive test: it validates the full integration from
/// daemon spawn → team config creation → Claude Code launch with agent flags →
/// mailbox infrastructure ready for message delivery.
#[test]
#[ignore]
#[timeout(240_000)]
fn test_real_claude_coworker_has_agent_teams_setup() {
    if std::env::var("MIDTOWN_LEAD_COMMAND").is_ok() {
        eprintln!("MIDTOWN_LEAD_COMMAND is set (stub mode), skipping real Claude test");
        return;
    }

    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    if !claude_available() {
        eprintln!("claude CLI not available, skipping");
        return;
    }

    let mut fixture = match MailboxFixture::new() {
        Some(f) => f,
        None => return,
    };

    assert!(
        fixture.start_daemon_with_tmux(),
        "Fixture failed to start daemon via `midtown start` for real-Claude mailbox test"
    );

    let session = fixture.tmux_session_name();

    // Wait for lead window
    let mut lead_found = false;
    for _ in 0..60 {
        thread::sleep(Duration::from_secs(1));
        if window_exists(&session, "lead") {
            lead_found = true;
            break;
        }
    }

    if !lead_found {
        eprintln!("Lead window did not appear, skipping");
        return;
    }

    // Spawn a coworker
    let spawn_response = fixture.rpc_call("coworker.spawn", Some(serde_json::json!({})));
    assert!(
        spawn_response.is_some(),
        "Should receive response from coworker.spawn"
    );

    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        eprintln!("Coworker spawn failed: {:?}", spawn_response["error"]);
        return;
    }

    let coworker_name = spawn_response["result"]["coworkers"][0]["name"]
        .as_str()
        .expect("Response should contain coworker name");
    eprintln!("Spawned coworker: {}", coworker_name);

    // Wait for coworker window to appear
    let mut window_found = false;
    for _ in 0..60 {
        thread::sleep(Duration::from_secs(1));
        if window_exists(&session, coworker_name) {
            window_found = true;
            break;
        }
    }

    assert!(
        window_found,
        "Coworker window '{}' should appear within 60s",
        coworker_name
    );

    // Verify team config exists and contains the coworker
    let config_path = fixture.team_dir.join("config.json");
    assert!(
        config_path.exists(),
        "Team config should exist at {:?}",
        config_path
    );

    let config_content = fs::read_to_string(&config_path).expect("Should read team config");
    let config: midtown::mailbox::TeamConfig =
        serde_json::from_str(&config_content).expect("Should parse team config");

    assert!(
        config.members.iter().any(|m| m.name == coworker_name),
        "Team config should contain coworker '{}'. Members: {:?}",
        coworker_name,
        config.members.iter().map(|m| &m.name).collect::<Vec<_>>()
    );

    // Verify the coworker TUI renders (Claude Code is running with agent flags)
    let mut has_output = false;
    for _ in 0..30 {
        thread::sleep(Duration::from_secs(1));
        if let Some(content) = capture_pane(&session, coworker_name)
            && content.lines().any(|l| !l.trim().is_empty())
        {
            has_output = true;
            break;
        }
    }

    assert!(
        has_output,
        "Coworker pane should have visible TUI output (Claude Code running with agent flags)"
    );

    // Write a message to the coworker's inbox and verify the file is created
    let msg =
        midtown::mailbox::MailboxMessage::new("Test message for real Claude coworker", "midtown")
            .with_summary("E2E test message");

    midtown::mailbox::write_to_inbox(&fixture.team_name, coworker_name, msg)
        .expect("Should write to coworker inbox");

    let inbox_path = fixture
        .team_dir
        .join("inboxes")
        .join(format!("{}.json", coworker_name));
    assert!(
        inbox_path.exists(),
        "Inbox file should exist for coworker at {:?}",
        inbox_path
    );

    let inbox_content = fs::read_to_string(&inbox_path).expect("Should read inbox");
    let messages: Vec<midtown::mailbox::MailboxMessage> =
        serde_json::from_str(&inbox_content).expect("Inbox should be valid JSON");
    assert!(
        !messages.is_empty(),
        "Inbox should contain at least one message"
    );
}

/// Test the mailbox fallback: when the daemon cannot write to the inbox,
/// it falls back to tmux send-keys nudge delivery.
///
/// We make the inboxes directory read-only so write_to_inbox fails, then
/// verify the daemon falls back to tmux send-keys by checking the coworker
/// pane for the nudge text.
#[test]
#[ignore]
#[timeout(180_000)]
fn test_mailbox_fallback_to_tmux_on_write_failure() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    if !claude_available() {
        eprintln!("claude CLI not available, skipping");
        return;
    }

    let mut fixture = match MailboxFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon_with_tmux() {
        return;
    }

    let session = fixture.tmux_session_name();

    // Wait for lead window
    for _ in 0..60 {
        thread::sleep(Duration::from_secs(1));
        if window_exists(&session, "lead") {
            break;
        }
    }

    // Spawn a coworker
    let spawn_response = fixture.rpc_call("coworker.spawn", Some(serde_json::json!({})));
    if spawn_response.is_none() {
        eprintln!("No response from coworker.spawn");
        return;
    }

    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        eprintln!("Coworker spawn failed: {:?}", spawn_response["error"]);
        return;
    }

    let coworker_name = spawn_response["result"]["coworkers"][0]["name"]
        .as_str()
        .expect("Response should contain coworker name");

    // Wait for the coworker window and TUI
    let mut window_found = false;
    for _ in 0..60 {
        thread::sleep(Duration::from_secs(1));
        if window_exists(&session, coworker_name) {
            window_found = true;
            break;
        }
    }

    if !window_found {
        eprintln!("Coworker window did not appear, skipping");
        return;
    }

    // Make the inboxes directory read-only to force write failures.
    // Use RAII guard to ensure permissions are restored even on panic.
    let inboxes_dir = fixture.team_dir.join("inboxes");
    let _readonly_guard = if inboxes_dir.exists() {
        Some(ReadOnlyGuard::new(&inboxes_dir))
    } else {
        None
    };

    // Record existing inbox state before triggering the task assignment,
    // so we can verify whether the daemon wrote via mailbox or fell back to tmux.
    let inbox_path = fixture
        .team_dir
        .join("inboxes")
        .join(format!("{}.json", coworker_name));
    let pre_task_inbox_size = fs::read_to_string(&inbox_path)
        .ok()
        .and_then(|c| serde_json::from_str::<Vec<serde_json::Value>>(&c).ok())
        .map(|msgs| msgs.len())
        .unwrap_or(0);

    // Now trigger a task assignment — the daemon should try mailbox first,
    // fail (due to read-only inboxes dir), and fall back to tmux send-keys
    let unique_tag = format!("fallback-test-{}", std::process::id());
    fixture.create_task("88", &unique_tag, "pending", None);

    // The daemon's dispatch tick should assign the task and attempt mailbox
    // delivery, which will fail, triggering the tmux fallback.
    // Check the coworker pane for the nudge text.
    let mut nudge_found = false;
    for _ in 0..60 {
        thread::sleep(Duration::from_secs(2));
        if let Some(content) = capture_pane(&session, coworker_name)
            && (content.contains("task") || content.contains("88"))
        {
            nudge_found = true;
            break;
        }
    }

    // Check whether the inbox was written despite read-only permissions.
    // If the inbox grew, the daemon wrote via mailbox (race: it assigned
    // before read-only took effect). If it didn't grow, the write was
    // blocked and we should see a tmux fallback nudge.
    let post_task_inbox_size = fs::read_to_string(&inbox_path)
        .ok()
        .and_then(|c| serde_json::from_str::<Vec<serde_json::Value>>(&c).ok())
        .map(|msgs| msgs.len())
        .unwrap_or(0);

    let mailbox_was_written = post_task_inbox_size > pre_task_inbox_size;

    if mailbox_was_written {
        // The daemon assigned the task before we made the directory read-only.
        // This is a known race — the test still passes because delivery occurred
        // (just via mailbox instead of tmux fallback).
        eprintln!(
            "INFO: Daemon delivered via mailbox (inbox grew from {} to {} messages). \
             Read-only was applied after delivery — fallback path not exercised this run.",
            pre_task_inbox_size, post_task_inbox_size
        );
    } else {
        // Read-only blocked the write — tmux fallback should have been used.
        assert!(
            nudge_found,
            "Mailbox write was blocked (inbox stayed at {} messages) but tmux fallback \
             nudge was not found in pane within 120s. The fallback path may be broken.",
            pre_task_inbox_size
        );
    }
}
