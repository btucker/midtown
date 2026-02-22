#![cfg(not(target_os = "linux"))]

//! Terminal E2E test for auth profile switching.
//!
//! Uses termwright to drive `midtown auth switch` in a PTY and verify that
//! codex global profile switching updates the provider current-profile marker.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use termwright::prelude::Terminal;

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

fn prepare_codex_profile(home: &Path, profile: &str) -> PathBuf {
    let profile_dir = home
        .join(".midtown")
        .join("auth")
        .join("providers")
        .join("codex")
        .join("profiles")
        .join(profile);
    fs::create_dir_all(&profile_dir).expect("create codex profile dir");
    profile_dir
}

fn auth_tree_snapshot(auth_root: &Path) -> String {
    fn walk(path: &Path, base: &Path, out: &mut Vec<String>, depth: usize) {
        if depth > 6 {
            return;
        }
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for entry_path in paths {
            if let Ok(rel) = entry_path.strip_prefix(base) {
                out.push(rel.display().to_string());
            }
            if entry_path.is_dir() {
                walk(&entry_path, base, out, depth + 1);
            }
        }
    }

    let mut rows = Vec::new();
    if auth_root.exists() {
        walk(auth_root, auth_root, &mut rows, 0);
    }
    rows.join("\n")
}

#[tokio::test]
async fn auth_switch_codex_updates_current_profile_marker() {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let temp = TempDir::new().expect("create temp dir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create fake HOME");

        let profile = "termwright@midtown.test";
        prepare_codex_profile(&home, profile);

        let binary = find_midtown_binary().expect("midtown binary not found");
        let binary = binary.to_string_lossy().to_string();
        let xdg_state_home = home.join(".local").join("state");

        let term = Terminal::builder()
            .size(120, 40)
            .working_dir(&home)
            .env("HOME", home.to_string_lossy().to_string())
            .env(
                "XDG_STATE_HOME",
                xdg_state_home.to_string_lossy().to_string(),
            )
            .spawn(&binary, &["auth", "--provider", "codex", "switch", profile])
            .await
            .expect("spawn midtown auth switch");

        let exit_code = term.wait_exit().await.expect("command should exit");
        assert_eq!(exit_code, 0, "auth switch should succeed");

        let screen_text = term.screen().await.text();
        let current_file = home
            .join(".midtown")
            .join("auth")
            .join("providers")
            .join("codex")
            .join("current");
        let selected = fs::read_to_string(&current_file).unwrap_or_else(|e| {
            panic!(
                "codex current profile marker should exist ({}).\nScreen:\n{}\nAuth tree:\n{}",
                e,
                screen_text,
                auth_tree_snapshot(&home.join(".midtown").join("auth"))
            )
        });
        assert_eq!(
            selected.trim(),
            profile,
            "codex profile should be activated"
        );

        assert!(
            screen_text.contains("Switched all projects to codex profile"),
            "expected success message on terminal screen, got:\n{}",
            screen_text
        );
    })
    .await;

    if result.is_err() {
        panic!("termwright auth switch e2e timed out after 30s");
    }
}
