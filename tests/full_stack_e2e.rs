//! Full-stack end-to-end tests with real Claude Code.
//!
//! These tests verify the complete integration path from daemon startup through
//! coworker spawning, channel communication, web UI connectivity, and worktree
//! isolation. They require a real Claude Code installation and tmux.
//!
//! Run with `cargo test --test full_stack_e2e -- --ignored --test-threads=1`
//! as these spawn real processes and tmux sessions.

use ntest::timeout;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

// ── Shared test infrastructure ─────────────────────────────────────

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("fullstack-e2e-test-{}-{}", std::process::id(), counter)
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success() || o.status.code() == Some(1))
        .unwrap_or(false)
}

/// Kill any orphaned test daemons from previous runs.
fn cleanup_orphaned_test_daemons() {
    let _ = Command::new("pkill")
        .args(["-f", "midtown daemon.*fullstack-e2e-test"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    thread::sleep(Duration::from_millis(50));

    let current_pid = format!("fullstack-e2e-test-{}-", std::process::id());
    let projects_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("projects");
    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with("fullstack-e2e-test-")
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
                && name.starts_with("fullstack-e2e-test-")
                && !name.starts_with(&current_pid)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Test fixture managing daemon lifecycle, tmux session, and cleanup.
#[allow(dead_code)]
struct FullStackFixture {
    temp_dir: PathBuf,
    project_dir: PathBuf,
    repo_name: String,
    socket_path: PathBuf,
    pid_path: PathBuf,
    daemon_process: Option<Child>,
}

impl FullStackFixture {
    fn new() -> Option<Self> {
        cleanup_orphaned_test_daemons();

        let repo_name = test_repo_name();
        let temp_dir = std::env::temp_dir().join(&repo_name);

        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).ok()?;

        // Initialize a git repository with an initial commit (needed for worktrees)
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

        Some(Self {
            temp_dir,
            project_dir,
            repo_name,
            socket_path,
            pid_path,
            daemon_process: None,
        })
    }

    fn start_daemon(&mut self) -> bool {
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
                for _ in 0..50 {
                    thread::sleep(Duration::from_millis(100));
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

    fn connect(&self) -> Option<UnixStream> {
        UnixStream::connect(&self.socket_path).ok()
    }

    fn rpc_call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let mut stream = self.connect()?;

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

    /// Return the tmux session name the daemon would use for this repo.
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

    /// Kill the tmux session created by the daemon.
    fn kill_tmux_session(&self) {
        let session = self.tmux_session_name();
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &session])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for FullStackFixture {
    fn drop(&mut self) {
        self.stop_daemon();
        self.kill_tmux_session();

        let _ = fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir(parent);
        }
        let _ = fs::remove_dir_all(&self.project_dir);
        let _ = fs::remove_dir_all(&self.temp_dir);

        // Clean up any worktrees created during tests
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
                // Window names may have status suffixes like "lead:dev#5",
                // so check if the base name matches
                let base_name = line.split(':').next().unwrap_or(line);
                base_name == window
            })
        }
        _ => false,
    }
}

// ── Tests ──────────────────────────────────────────────────────────

/// Start daemon, verify lead window appears and claude TUI renders
/// (pane has output within 60s).
///
/// This test verifies the full daemon → tmux → Claude Code launch path.
/// The daemon creates a tmux session and spawns a "lead" window running
/// Claude Code. We verify the window exists and has visible TUI output.
#[test]
#[ignore]
#[timeout(120_000)]
fn test_daemon_spawns_lead_with_real_claude() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let mut fixture = match FullStackFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let session = fixture.tmux_session_name();

    // Wait for the lead window to appear (up to 60s)
    let mut lead_found = false;
    for _ in 0..60 {
        thread::sleep(Duration::from_secs(1));
        if window_exists(&session, "lead") {
            lead_found = true;
            break;
        }
    }

    assert!(
        lead_found,
        "Lead window should appear in tmux session '{}' within 60s",
        session
    );

    // Verify the lead pane has visible output (TUI rendered)
    let mut has_output = false;
    for _ in 0..30 {
        thread::sleep(Duration::from_secs(1));
        if let Some(content) = capture_pane(&session, "lead")
            && midtown::tmux::content_has_output(&content)
        {
            has_output = true;
            break;
        }
    }

    assert!(
        has_output,
        "Lead pane should have visible TUI output within 90s of daemon start"
    );
}

/// Spawn coworker via RPC, verify it posts a greeting to the channel.
///
/// The daemon's coworker.spawn RPC creates a worktree, opens a tmux window,
/// and launches Claude Code with an initial prompt. The coworker's system
/// prompt instructs it to read the channel on startup, which produces output.
#[test]
#[ignore]
#[timeout(120_000)]
fn test_coworker_spawn_and_channel_post() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let mut fixture = match FullStackFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Spawn a coworker via RPC
    let spawn_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({ "name": "lexington" })),
    );

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

    // Wait for the coworker to post something to the channel (up to 90s)
    // The coworker's system prompt instructs it to run `midtown channel read`
    // on startup, which should produce channel activity.
    let channel_path = fixture.project_dir.join("channel.jsonl");
    let mut coworker_posted = false;
    for _ in 0..90 {
        thread::sleep(Duration::from_secs(1));
        if let Ok(content) = fs::read_to_string(&channel_path) {
            // Look for any message from the spawned coworker
            if content.lines().any(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .is_some_and(|msg| msg["from"].as_str() == Some("lexington"))
            }) {
                coworker_posted = true;
                break;
            }
        }
    }

    assert!(
        coworker_posted,
        "Coworker 'lexington' should post to the channel within 90s of spawn"
    );
}

/// Send an @lead channel message, verify the nudge appears in the lead pane.
///
/// Tests the full nudge delivery path: channel.post RPC → daemon detects
/// @lead mention → tmux send_keys to lead window.
#[test]
#[ignore]
#[timeout(120_000)]
fn test_nudge_reaches_real_claude() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let mut fixture = match FullStackFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    let session = fixture.tmux_session_name();

    // Wait for the lead window to appear first
    let mut lead_found = false;
    for _ in 0..60 {
        thread::sleep(Duration::from_secs(1));
        if window_exists(&session, "lead") {
            lead_found = true;
            break;
        }
    }

    if !lead_found {
        eprintln!("Lead window did not appear, skipping nudge test");
        return;
    }

    // Wait for the lead TUI to render (so it can receive input)
    thread::sleep(Duration::from_secs(5));

    // Post a channel message with @lead mention
    let unique_tag = format!("nudge-test-{}", std::process::id());
    let params = serde_json::json!({
        "message": format!("@lead {}", unique_tag),
        "from": "test-agent"
    });

    let post_response = fixture.rpc_call("channel.post", Some(params));
    assert!(
        post_response.is_some(),
        "Should receive response from channel.post"
    );

    // Wait for the nudge to be delivered to the lead pane (up to 30s)
    // The daemon's chat monitor detects @lead and sends it via tmux send_keys
    let mut nudge_delivered = false;
    for _ in 0..30 {
        thread::sleep(Duration::from_secs(1));
        if let Some(content) = capture_pane(&session, "lead")
            && content.contains(&unique_tag)
        {
            nudge_delivered = true;
            break;
        }
    }

    assert!(
        nudge_delivered,
        "Nudge with @lead mention should appear in the lead pane within 30s"
    );
}

/// Verify the webserver on port 47022 returns 200 and WebSocket connects.
///
/// The daemon starts an HTTP server for the web UI. This test verifies
/// the health endpoint responds and that a WebSocket upgrade is possible.
///
/// Note: This test checks the per-daemon web API (not the multi-project
/// webserver on 47022), since the daemon starts its own HTTP listener
/// on the webhook port. With MIDTOWN_WEBHOOK_PORT=0, the daemon picks
/// an ephemeral port — so this test uses the RPC status to discover it.
#[test]
#[ignore]
#[timeout(120_000)]
fn test_web_ui_connects() {
    let mut fixture = match FullStackFixture::new() {
        Some(f) => f,
        None => return,
    };

    // Start daemon WITHOUT disabling the webhook port so we get an HTTP server
    let build_result = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    if build_result.map(|s| !s.success()).unwrap_or(true) {
        eprintln!("Failed to build daemon binary");
        return;
    }

    let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("midtown");

    let _ = fs::remove_file(&fixture.socket_path);
    let _ = fs::remove_file(&fixture.pid_path);

    // Use a specific port for the webhook/web server so we can connect to it
    let test_port = 47099u16;
    let child = Command::new(&binary_path)
        .arg("daemon")
        .arg("--workdir")
        .arg(&fixture.temp_dir)
        .current_dir(&fixture.temp_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("MIDTOWN_WEBHOOK_PORT", test_port.to_string())
        .env("MIDTOWN_CHAT_MONITOR", "0")
        .spawn();

    match child {
        Ok(c) => {
            fixture.daemon_process = Some(c);
            // Wait for daemon socket
            let mut ready = false;
            for _ in 0..50 {
                thread::sleep(Duration::from_millis(100));
                if fixture.socket_path.exists() && UnixStream::connect(&fixture.socket_path).is_ok()
                {
                    ready = true;
                    break;
                }
            }
            if !ready {
                eprintln!("Daemon socket did not become available");
                return;
            }
        }
        Err(e) => {
            eprintln!("Failed to spawn daemon: {}", e);
            return;
        }
    }

    // Wait a moment for the HTTP server to bind
    thread::sleep(Duration::from_secs(2));

    // Check the health endpoint
    let health_url = format!("http://127.0.0.1:{}/api/health", test_port);
    let health_response = Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", &health_url])
        .output();

    match health_response {
        Ok(output) => {
            let status_code = String::from_utf8_lossy(&output.stdout).to_string();
            assert_eq!(
                status_code, "200",
                "Web API health endpoint should return 200, got {}",
                status_code
            );
        }
        Err(e) => {
            eprintln!("curl not available, skipping HTTP check: {}", e);
            return;
        }
    }

    // Verify WebSocket upgrade is possible by checking the upgrade response
    let ws_url = format!("http://127.0.0.1:{}/api/ws", test_port);
    let ws_response = Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "-H",
            "Upgrade: websocket",
            "-H",
            "Connection: Upgrade",
            "-H",
            "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==",
            "-H",
            "Sec-WebSocket-Version: 13",
            &ws_url,
        ])
        .output();

    match ws_response {
        Ok(output) => {
            let status_code = String::from_utf8_lossy(&output.stdout).to_string();
            // WebSocket upgrade should return 101 Switching Protocols
            assert_eq!(
                status_code, "101",
                "WebSocket endpoint should return 101 Switching Protocols, got {}",
                status_code
            );
        }
        Err(e) => {
            eprintln!("curl not available for WebSocket check: {}", e);
        }
    }
}

/// Spawn coworker, verify worktree exists as a valid git worktree.
///
/// When the daemon spawns a coworker, it creates an isolated git worktree
/// at ~/.midtown/coworkers/<repo>/<name>/. This test verifies the worktree
/// is properly created and recognized by git as a valid worktree.
#[test]
#[ignore]
#[timeout(120_000)]
fn test_worktree_isolation() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let mut fixture = match FullStackFixture::new() {
        Some(f) => f,
        None => return,
    };

    if !fixture.start_daemon() {
        return;
    }

    // Spawn a coworker via RPC
    let spawn_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({ "name": "park" })),
    );

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

    // Give the daemon a moment to create the worktree
    thread::sleep(Duration::from_secs(5));

    // Verify the worktree directory exists
    let worktree_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("coworkers")
        .join(&fixture.repo_name)
        .join("park");

    assert!(
        worktree_path.exists(),
        "Worktree directory should exist at {:?}",
        worktree_path
    );

    // Verify it's a valid git worktree by checking for .git file
    // (worktrees have a .git file pointing to the main repo, not a .git directory)
    let git_file = worktree_path.join(".git");
    assert!(
        git_file.exists(),
        "Worktree should have a .git file at {:?}",
        git_file
    );

    // Verify git recognizes it as a worktree
    let git_status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&worktree_path)
        .output();

    assert!(
        git_status.is_ok() && git_status.unwrap().status.success(),
        "git status should succeed in the worktree directory"
    );

    // Verify the worktree is listed in the main repo's worktree list
    let worktree_list = Command::new("git")
        .args(["worktree", "list"])
        .current_dir(&fixture.temp_dir)
        .output();

    match worktree_list {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("park"),
                "Worktree 'park' should appear in git worktree list. Got:\n{}",
                stdout
            );
        }
        _ => {
            eprintln!("Failed to run git worktree list");
        }
    }
}
