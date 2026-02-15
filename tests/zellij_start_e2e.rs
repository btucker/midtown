//! End-to-end test that validates `midtown start` creates a real Zellij session.
//!
//! This guards the startup path where Midtown launches the Lead session via
//! Zellij. It intentionally avoids requiring Claude by setting
//! `MIDTOWN_LEAD_COMMAND` to a stub command.

use ntest::timeout;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_repo_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("zellij-start-e2e-{}-{}", std::process::id(), counter)
}

fn zellij_available() -> bool {
    Command::new("zellij")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn midtown_binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_midtown"))
}

fn setup_repo() -> Option<(PathBuf, String)> {
    let repo_name = test_repo_name();
    let temp_dir = std::env::temp_dir().join(&repo_name);
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).ok()?;

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

    let _ = Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&temp_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(&temp_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

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

    Some((temp_dir, repo_name))
}

#[test]
#[ignore] // Requires built binary and zellij
#[timeout(120_000)]
fn test_midtown_start_creates_zellij_session() {
    assert!(zellij_available(), "zellij must be installed for this test");

    let binary_path = midtown_binary_path();
    assert!(
        binary_path.exists(),
        "test-built midtown binary missing at {}",
        binary_path.display()
    );

    let (temp_dir, repo_name) = setup_repo().expect("failed to initialize test git repo");
    let session = format!("midtown-{}", repo_name);

    // Ensure no stale session is present from previous failed runs.
    let _ = Command::new("zellij")
        .args(["kill-session", &session])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let start_output = Command::new(&binary_path)
        .arg("start")
        .current_dir(&temp_dir)
        .env("MIDTOWN_LEAD_COMMAND", "sleep 120")
        .env("MIDTOWN_WEBHOOK_PORT", "0")
        .env("MIDTOWN_CHAT_MONITOR", "0")
        .output()
        .expect("Failed to run `midtown start`");

    assert!(
        start_output.status.success(),
        "`midtown start` failed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&start_output.stdout),
        String::from_utf8_lossy(&start_output.stderr)
    );

    let mut session_found = false;
    for _ in 0..40 {
        if midtown::process::zellij_session_exists(&session) {
            session_found = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    if !session_found {
        let sessions = Command::new("zellij")
            .args(["list-sessions", "--no-formatting"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_else(|| "<failed to list sessions>".to_string());
        panic!(
            "Expected zellij session '{}' after `midtown start`, but it was not found.\nSessions:\n{}",
            session, sessions
        );
    }

    // Validate generated layout contract: chat pane on the left, lead on the right,
    // and bottom keybinding hints bar enabled.
    let layout_path = midtown::paths::midtown_base_dir()
        .join("layouts")
        .join(format!("{}.kdl", repo_name));
    let layout = fs::read_to_string(&layout_path).expect("expected generated zellij layout");

    let chat_idx = layout
        .find("command \"midtown\"")
        .expect("layout missing chat pane command");
    let lead_idx = layout
        .find("command \"bash\"")
        .expect("layout missing lead pane launcher");
    assert!(
        chat_idx < lead_idx,
        "default layout should keep chat left and lead right. Layout:\n{}",
        layout
    );
    assert!(
        layout.contains("plugin location=\"zellij:status-bar\""),
        "layout should include bottom status-bar plugin for keybinding hints. Layout:\n{}",
        layout
    );

    // Best-effort cleanup so this test doesn't leak daemon/session resources.
    let _ = Command::new(&binary_path)
        .arg("stop")
        .current_dir(&temp_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("zellij")
        .args(["kill-session", &session])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = fs::remove_dir_all(&temp_dir);
}
