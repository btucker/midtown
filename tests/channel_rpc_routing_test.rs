//! E2E tests for channel RPC routing — specifically that channel.read respects
//! the `channel` parameter so that channel leads read from their topic channel
//! instead of the main channel.
//!
//! Regression tests for: user messages sent to topic channels from the web UI
//! not being visible to channel leads because channel.read always read from the
//! main channel.
//!
//! Run with `cargo test --test channel_rpc_routing_test -- --ignored` as these
//! spawn a real daemon.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("channel-rpc-test-{}-{}", std::process::id(), counter)
}

struct DaemonFixture {
    temp_dir: PathBuf,
    project_dir: PathBuf,
    socket_path: PathBuf,
    daemon_process: Option<Child>,
}

impl DaemonFixture {
    fn new() -> Self {
        let repo_name = test_repo_name();
        let temp_dir = std::env::temp_dir().join(&repo_name);
        fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

        for (args, desc) in [
            (vec!["init"], "init"),
            (vec!["config", "user.name", "Test User"], "config name"),
            (
                vec!["config", "user.email", "test@example.com"],
                "config email",
            ),
        ] {
            Command::new("git")
                .args(&args)
                .current_dir(&temp_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap_or_else(|_| panic!("Failed to git {}", desc));
        }

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

        let project_dir = dirs::home_dir()
            .expect("home dir")
            .join(".midtown")
            .join("projects")
            .join(&repo_name);

        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .expect("home dir")
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
            socket_path,
            daemon_process: None,
        }
    }

    fn start_daemon(&mut self) {
        let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("midtown");

        assert!(
            binary_path.exists(),
            "Debug binary not found at {:?}. Run `cargo build` first.",
            binary_path
        );

        let _ = fs::remove_file(&self.socket_path);

        let daemon = Command::new(&binary_path)
            .args(["daemon", "--workdir", self.temp_dir.to_str().unwrap()])
            .current_dir(&self.temp_dir)
            .env("MIDTOWN_WEBHOOK_PORT", "0")
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("Failed to start daemon");

        self.daemon_process = Some(daemon);

        for _ in 0..300 {
            thread::sleep(Duration::from_millis(200));
            if self.socket_path.exists() && UnixStream::connect(&self.socket_path).is_ok() {
                return;
            }
        }
        panic!("Daemon socket did not become available within 60 seconds");
    }

    fn rpc_call(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let mut stream =
            UnixStream::connect(&self.socket_path).expect("Failed to connect to daemon");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set write timeout");

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        });

        writeln!(stream, "{}", request).expect("Failed to write request");

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("Failed to read response");
        serde_json::from_str(&line).expect("Failed to parse response")
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        if let Some(mut daemon) = self.daemon_process.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        let _ = fs::remove_dir_all(&self.temp_dir);
        let _ = fs::remove_dir_all(&self.project_dir);
        if let Some(parent) = self.socket_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

/// Verify that channel.read with a channel parameter returns messages from
/// the topic channel, not the main channel.
///
/// Regression test: channel leads calling `midtown channel read` always got
/// main channel messages instead of their topic channel messages, causing them
/// to miss user messages posted to their topic channel from the web UI.
#[test]
#[ignore] // E2E test - requires daemon
fn test_channel_read_topic_channel_routing() {
    let mut fixture = DaemonFixture::new();
    fixture.start_daemon();

    // Post a message to the topic channel (simulating a web UI user message)
    let post_topic = fixture.rpc_call(
        "channel.post",
        serde_json::json!({
            "message": "hello from topic channel",
            "from": "user",
            "channel": "auth"
        }),
    );
    assert!(
        post_topic.get("error").is_none(),
        "channel.post to topic channel should succeed: {:?}",
        post_topic
    );

    // Post a different message to the main channel
    let post_main = fixture.rpc_call(
        "channel.post",
        serde_json::json!({
            "message": "hello from main channel",
            "from": "user"
        }),
    );
    assert!(
        post_main.get("error").is_none(),
        "channel.post to main channel should succeed: {:?}",
        post_main
    );

    // Read from the topic channel — should return only the topic message
    let read_topic = fixture.rpc_call(
        "channel.read",
        serde_json::json!({
            "all": true,
            "channel": "auth"
        }),
    );
    assert!(
        read_topic.get("error").is_none(),
        "channel.read from topic channel should succeed: {:?}",
        read_topic
    );

    let topic_messages = read_topic["result"]["messages"]
        .as_array()
        .expect("messages should be an array");
    assert_eq!(
        topic_messages.len(),
        1,
        "Topic channel should have exactly 1 message, got: {:?}",
        topic_messages
    );
    assert!(
        topic_messages[0]["message"]
            .as_str()
            .unwrap_or("")
            .contains("hello from topic channel"),
        "Topic channel message should be the one posted to 'auth', got: {:?}",
        topic_messages[0]
    );

    // Read from the main channel (no channel param) — should return only the main message
    let read_main = fixture.rpc_call(
        "channel.read",
        serde_json::json!({
            "all": true
        }),
    );
    assert!(
        read_main.get("error").is_none(),
        "channel.read from main channel should succeed: {:?}",
        read_main
    );

    let main_messages = read_main["result"]["messages"]
        .as_array()
        .expect("messages should be an array");
    assert_eq!(
        main_messages.len(),
        1,
        "Main channel should have exactly 1 message, got: {:?}",
        main_messages
    );
    assert!(
        main_messages[0]["message"]
            .as_str()
            .unwrap_or("")
            .contains("hello from main channel"),
        "Main channel message should be the one posted without channel param, got: {:?}",
        main_messages[0]
    );
}
