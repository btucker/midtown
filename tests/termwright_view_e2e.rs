#![cfg(not(target_os = "linux"))]

//! Terminal E2E test for `midtown view`.
//!
//! Validates that `midtown view` can launch the chat UI in a PTY and exit
//! cleanly via keyboard input.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;
use termwright::prelude::{Key, Terminal};

fn find_midtown_binary() -> Option<PathBuf> {
    if let Some(bin) = option_env!("CARGO_BIN_EXE_midtown") {
        let path = PathBuf::from(bin);
        if path.exists() {
            return Some(path);
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    [
        manifest_dir.join("target/debug/midtown"),
        manifest_dir.join("target/release/midtown"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

fn init_git_repo(repo: &Path) {
    let status = Command::new("git")
        .args(["init"])
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git init");
    assert!(status.success(), "git init should succeed");

    let _ = Command::new("git")
        .args(["config", "user.email", "test@midtown.local"])
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("git")
        .args(["config", "user.name", "Midtown Test"])
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let status = Command::new("git")
        .args(["commit", "--allow-empty", "-m", "Initial commit"])
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run initial commit");
    assert!(status.success(), "initial commit should succeed");
}

struct MidtownStopGuard {
    binary: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    xdg_state_home: PathBuf,
}

impl Drop for MidtownStopGuard {
    fn drop(&mut self) {
        let _ = Command::new(&self.binary)
            .arg("stop")
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.xdg_state_home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[tokio::test]
async fn midtown_view_launches_chat_and_exits_with_ctrl_q() {
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        let temp = TempDir::new().expect("create temp dir");
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&home).expect("create fake HOME");
        fs::create_dir_all(&repo).expect("create fake repo dir");
        init_git_repo(&repo);

        let binary = find_midtown_binary().expect("midtown binary not found");
        let xdg_state_home = home.join(".local").join("state");
        let binary_str = binary.to_string_lossy().to_string();

        let _stop_guard = MidtownStopGuard {
            binary: binary.clone(),
            repo: repo.clone(),
            home: home.clone(),
            xdg_state_home: xdg_state_home.clone(),
        };

        let term = Terminal::builder()
            .size(140, 45)
            .working_dir(&repo)
            .env("HOME", home.to_string_lossy().to_string())
            .env(
                "XDG_STATE_HOME",
                xdg_state_home.to_string_lossy().to_string(),
            )
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .env("MIDTOWN_WEBHOOK_PORT", "0")
            .spawn(&binary_str, &["view"])
            .await
            .expect("spawn midtown view");

        term.expect("Board")
            .timeout(Duration::from_secs(25))
            .await
            .expect("chat UI should render Board pane");

        term.send_key(Key::Ctrl('q'))
            .await
            .expect("send Ctrl+Q to exit chat");

        let exit_code = term.wait_exit().await.expect("midtown view should exit");
        assert_eq!(exit_code, 0, "midtown view should exit cleanly");

        let screen = term.screen().await;
        assert!(
            screen.contains("Exited chat session"),
            "expected graceful exit message, got:\n{}",
            screen.text()
        );
    })
    .await;

    if result.is_err() {
        panic!("termwright midtown view e2e timed out after 60s");
    }
}
