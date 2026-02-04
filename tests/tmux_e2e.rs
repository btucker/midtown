//! End-to-end tests for tmux window and pane management.
//!
//! Covers:
//! - Window rename and list_windows suffix stripping
//! - Lead window spawn and respawn lifecycle
//! - Window lifecycle (process exit → window death detection)
//! - PTY isolation across windows
//! - Chat pane idempotency (setup_chat_pane must not duplicate splits)
//!
//! Run with `cargo test --test tmux_e2e -- --ignored` as these require tmux.

use ntest::timeout;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

// ── Shared test helpers ────────────────────────────────────────────

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_session_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("midtown-tmux-test-{}-{}", std::process::id(), counter)
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success() || o.status.code() == Some(1))
        .unwrap_or(false)
}

fn create_test_session(session: &str) -> bool {
    Command::new("tmux")
        .args(["new-session", "-d", "-s", session, "-x", "200", "-y", "50"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn create_test_window(session: &str, window: &str) -> bool {
    Command::new("tmux")
        .args(["new-window", "-t", session, "-n", window])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn rename_tmux_window(session: &str, old_name: &str, new_name: &str) -> bool {
    let target = format!("{}:{}", session, old_name);
    Command::new("tmux")
        .args(["rename-window", "-t", &target, new_name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn kill_test_session(session: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

fn kill_window(session: &str, window: &str) {
    let target = format!("{}:{}", session, window);
    let _ = Command::new("tmux")
        .args(["kill-window", "-t", &target])
        .status();
}

fn pane_count(session: &str, window: &str) -> usize {
    let target = format!("{}:{}", session, window);
    let output = Command::new("tmux")
        .args(["list-panes", "-t", &target, "-F", "#{pane_index}"])
        .output()
        .expect("list-panes failed");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .count()
}

fn pane_widths(session: &str, window: &str) -> Vec<u32> {
    let target = format!("{}:{}", session, window);
    let output = Command::new("tmux")
        .args(["list-panes", "-t", &target, "-F", "#{pane_width}"])
        .output()
        .expect("list-panes failed");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.parse().ok())
        .collect()
}

fn session_ptys(session: &str) -> Vec<(String, String)> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-s",
            "-t",
            session,
            "-F",
            "#{window_name} #{pane_tty}",
        ])
        .output()
        .expect("list-panes failed");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let parts: Vec<&str> = l.splitn(2, ' ').collect();
            (
                parts[0].to_string(),
                parts.get(1).unwrap_or(&"").to_string(),
            )
        })
        .collect()
}

/// RAII guard that kills the tmux session on drop.
struct SessionCleanup {
    session: String,
}

impl Drop for SessionCleanup {
    fn drop(&mut self) {
        kill_test_session(&self.session);
    }
}

// ── Window rename / list_windows ───────────────────────────────────

#[test]
#[timeout(30_000)]
#[ignore]
fn test_list_windows_strips_status_suffix() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    assert!(create_test_window(&session, "york"));
    thread::sleep(Duration::from_millis(100));

    assert!(rename_tmux_window(&session, "york", "york:done#204"));
    thread::sleep(Duration::from_millis(100));

    let windows = midtown::tmux::list_windows(&session).expect("list_windows failed");

    assert!(
        windows.contains(&"york".to_string()),
        "Expected list_windows to return base name 'york', got: {:?}",
        windows
    );
    assert!(
        !windows.contains(&"york:done#204".to_string()),
        "list_windows should NOT return the suffixed name, got: {:?}",
        windows
    );
}

#[test]
#[timeout(30_000)]
#[ignore]
fn test_list_windows_deduplicates_after_stripping() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    assert!(create_test_window(&session, "amsterdam"));
    thread::sleep(Duration::from_millis(100));

    rename_tmux_window(&session, "amsterdam", "amsterdam:dev#5");
    thread::sleep(Duration::from_millis(100));

    let windows = midtown::tmux::list_windows(&session).expect("list_windows failed");

    let amsterdam_count = windows.iter().filter(|w| w.as_str() == "amsterdam").count();
    assert_eq!(
        amsterdam_count, 1,
        "Expected exactly one 'amsterdam', got {} in {:?}",
        amsterdam_count, windows
    );
}

#[test]
#[timeout(30_000)]
#[ignore]
fn test_list_all_windows_includes_lead() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let default_window = format!("{}:0", session);
    let _ = Command::new("tmux")
        .args(["rename-window", "-t", &default_window, "lead"])
        .status();
    thread::sleep(Duration::from_millis(100));

    assert!(create_test_window(&session, "york"));
    assert!(create_test_window(&session, "amsterdam"));
    thread::sleep(Duration::from_millis(100));

    let all_windows = midtown::tmux::list_all_windows(&session).expect("list_all_windows failed");
    let coworker_windows = midtown::tmux::list_windows(&session).expect("list_windows failed");

    assert!(
        all_windows.contains(&"lead".to_string()),
        "list_all_windows should include 'lead', got: {:?}",
        all_windows
    );
    assert!(
        all_windows.contains(&"york".to_string()),
        "list_all_windows should include 'york', got: {:?}",
        all_windows
    );
    assert!(
        !coworker_windows.contains(&"lead".to_string()),
        "list_windows should NOT include 'lead', got: {:?}",
        coworker_windows
    );
}

#[test]
#[timeout(30_000)]
#[ignore]
fn test_list_all_windows_deduplicates() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    assert!(create_test_window(&session, "york"));
    thread::sleep(Duration::from_millis(100));

    rename_tmux_window(&session, "york", "york:done#204");
    thread::sleep(Duration::from_millis(100));

    let windows = midtown::tmux::list_all_windows(&session).expect("list_all_windows failed");

    let york_count = windows.iter().filter(|w| w.as_str() == "york").count();
    assert_eq!(
        york_count, 1,
        "Expected exactly one 'york' in list_all_windows, got {} in {:?}",
        york_count, windows
    );
}

#[test]
#[timeout(30_000)]
#[ignore]
fn test_rename_window_works_after_previous_rename() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    assert!(create_test_window(&session, "york"));
    thread::sleep(Duration::from_millis(100));

    midtown::tmux::rename_window(&session, "york", Some("developing task 5"))
        .expect("First rename failed");
    thread::sleep(Duration::from_millis(100));

    let raw_output = Command::new("tmux")
        .args(["list-windows", "-t", &session, "-F", "#{window_name}"])
        .output()
        .expect("list-windows failed");
    let raw_names: Vec<String> = String::from_utf8_lossy(&raw_output.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect();
    assert!(
        raw_names.iter().any(|n| n.starts_with("york:")),
        "Expected a renamed window starting with 'york:', got: {:?}",
        raw_names
    );

    midtown::tmux::rename_window(&session, "york", Some("testing task 5"))
        .expect("Second rename failed");
    thread::sleep(Duration::from_millis(100));

    let raw_output2 = Command::new("tmux")
        .args(["list-windows", "-t", &session, "-F", "#{window_name}"])
        .output()
        .expect("list-windows failed");
    let raw_names2: Vec<String> = String::from_utf8_lossy(&raw_output2.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect();

    assert!(
        raw_names2.iter().any(|n| n.contains("test")),
        "Expected window with 'test' status after second rename, got: {:?}",
        raw_names2
    );
}

// ── Lead spawn / respawn ───────────────────────────────────────────

#[test]
#[ignore]
#[timeout(30_000)]
fn test_spawn_lead_creates_window() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();

    let result = midtown::tmux::spawn_lead(
        &session,
        &temp.path().to_string_lossy(),
        "lead-spawn-test",
        &[],
    );

    assert!(result.is_ok(), "spawn_lead failed: {:?}", result.err());

    let exists =
        midtown::tmux::window_exists(&session, "lead").expect("Failed to check window existence");
    assert!(exists, "Lead window should exist after spawn_lead");
}

#[test]
#[ignore]
#[timeout(30_000)]
fn test_spawn_lead_window_has_correct_name() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();

    midtown::tmux::spawn_lead(
        &session,
        &temp.path().to_string_lossy(),
        "lead-name-test",
        &[],
    )
    .expect("spawn_lead failed");

    let output = Command::new("tmux")
        .args(["list-windows", "-t", &session, "-F", "#{window_name}"])
        .output()
        .expect("Failed to list windows");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let window_names: Vec<&str> = stdout.lines().collect();
    assert!(
        window_names.iter().any(|n| n.to_lowercase() == "lead"),
        "Expected 'lead' window in session, got: {:?}",
        window_names
    );
}

#[test]
#[ignore]
#[timeout(30_000)]
fn test_respawn_lead_after_kill() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();
    let workdir = temp.path().to_string_lossy().to_string();

    midtown::tmux::spawn_lead(&session, &workdir, "lead-respawn-test", &[])
        .expect("Initial spawn_lead failed");

    assert!(
        midtown::tmux::window_exists(&session, "lead").unwrap(),
        "Lead should exist after initial spawn"
    );

    kill_window(&session, "lead");
    thread::sleep(Duration::from_millis(200));

    assert!(
        !midtown::tmux::window_exists(&session, "lead").unwrap(),
        "Lead should be gone after kill"
    );

    midtown::tmux::spawn_lead(&session, &workdir, "lead-respawn-test", &[])
        .expect("Respawn spawn_lead failed");

    assert!(
        midtown::tmux::window_exists(&session, "lead").unwrap(),
        "Lead should exist after respawn"
    );
}

#[test]
#[ignore]
#[timeout(30_000)]
fn test_check_and_respawn_lead_recreates_killed_window() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();
    let workdir = temp.path().to_string_lossy().to_string();

    midtown::tmux::spawn_lead(&session, &workdir, "lead-check-test", &[])
        .expect("Initial spawn_lead failed");

    kill_window(&session, "lead");
    thread::sleep(Duration::from_millis(200));

    assert!(
        !midtown::tmux::window_exists(&session, "lead").unwrap(),
        "Lead should be gone after kill"
    );

    midtown::tmux::spawn_lead(&session, &workdir, "lead-check-test", &[]).expect("Respawn failed");

    assert!(
        midtown::tmux::window_exists(&session, "lead").unwrap(),
        "Lead should be recreated after respawn"
    );
}

#[test]
#[ignore]
#[timeout(30_000)]
fn test_no_respawn_when_session_gone() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    // Don't create the session — verify spawn_lead fails gracefully

    let temp = tempfile::tempdir().unwrap();
    let result = midtown::tmux::spawn_lead(
        &session,
        &temp.path().to_string_lossy(),
        "lead-no-session-test",
        &[],
    );

    assert!(
        result.is_err(),
        "spawn_lead should fail when session doesn't exist"
    );
}

/// Unit test (no tmux needed): task_list_id_for_repo format.
#[test]
fn test_lead_command_no_resume_no_session_id() {
    let derived = midtown::paths::task_list_id_for_repo("test-project");
    assert_eq!(derived, "midtown-test-project");
}

// ── Window lifecycle ───────────────────────────────────────────────

#[test]
#[ignore]
#[timeout(15_000)]
fn test_window_dies_when_command_exits() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();

    midtown::tmux::create_window(
        &session,
        "shortlived",
        &temp.path().to_string_lossy(),
        Some("export MIDTOWN_AGENT='test'; exec sleep 0.2"),
    )
    .expect("create_window failed");

    thread::sleep(Duration::from_millis(800));

    let exists =
        midtown::tmux::window_exists(&session, "shortlived").expect("window_exists failed");

    assert!(
        !exists,
        "Window should die when the command exits — if it doesn't, \
         the daemon can't detect dead coworkers to respawn them"
    );
}

#[test]
#[ignore]
#[timeout(15_000)]
fn test_window_alive_while_command_runs() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();

    midtown::tmux::create_window(
        &session,
        "longrunning",
        &temp.path().to_string_lossy(),
        Some("sleep 300"),
    )
    .expect("create_window failed");

    thread::sleep(Duration::from_millis(300));

    let exists =
        midtown::tmux::window_exists(&session, "longrunning").expect("window_exists failed");

    assert!(exists, "Window should stay alive while command is running");
}

// ── PTY isolation ──────────────────────────────────────────────────

/// Each tmux window must get a unique PTY device.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_each_window_gets_unique_pty() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().to_string();

    for name in ["lead", "park", "lexington", "madison"] {
        midtown::tmux::create_window(&session, name, &dir, Some("sleep 300"))
            .unwrap_or_else(|e| panic!("create_window({}) failed: {}", name, e));
    }
    thread::sleep(Duration::from_millis(300));

    let ptys = session_ptys(&session);

    let pty_paths: Vec<&str> = ptys
        .iter()
        .filter(|(name, _)| ["lead", "park", "lexington", "madison"].contains(&name.as_str()))
        .map(|(_, pty)| pty.as_str())
        .collect();

    assert_eq!(pty_paths.len(), 4, "Expected 4 windows, got: {:?}", ptys);

    let mut unique = pty_paths.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        pty_paths.len(),
        "PTYs must be unique across windows! Got: {:?}",
        ptys
    );
}

// ── Chat pane idempotency ──────────────────────────────────────────

/// setup_chat_pane must be idempotent — calling it multiple times should NOT
/// create additional panes beyond the expected lead + chat layout.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_setup_chat_pane_is_idempotent() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let default_target = format!("{}:0", session);
    let _ = Command::new("tmux")
        .args(["rename-window", "-t", &default_target, "lead"])
        .status();
    thread::sleep(Duration::from_millis(100));

    midtown::tmux::setup_chat_pane(&session);
    thread::sleep(Duration::from_millis(200));

    let count_after_first = pane_count(&session, "lead");

    midtown::tmux::setup_chat_pane(&session);
    thread::sleep(Duration::from_millis(200));

    let count_after_second = pane_count(&session, "lead");

    assert_eq!(
        count_after_first, count_after_second,
        "setup_chat_pane is not idempotent! First call: {} panes, second call: {} panes. \
         Each call to ensure_lead_has_settings adds an extra pane, progressively \
         shrinking the lead's terminal until the TUI can't render.",
        count_after_first, count_after_second
    );
}

/// Verify that the lead pane width doesn't shrink across multiple reinits.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_lead_pane_width_stable_across_reinits() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let default_target = format!("{}:0", session);
    let _ = Command::new("tmux")
        .args(["rename-window", "-t", &default_target, "lead"])
        .status();
    thread::sleep(Duration::from_millis(100));

    midtown::tmux::setup_chat_pane(&session);
    thread::sleep(Duration::from_millis(200));

    let initial_widths = pane_widths(&session, "lead");
    let initial_lead_width = initial_widths[0];

    for i in 1..=3 {
        midtown::tmux::setup_chat_pane(&session);
        thread::sleep(Duration::from_millis(200));

        let widths = pane_widths(&session, "lead");
        let lead_width = widths[0];

        assert_eq!(
            lead_width, initial_lead_width,
            "Lead pane width shrank from {} to {} after {} reinit(s). \
             The TUI will eventually become too narrow to render, appearing hung.",
            initial_lead_width, lead_width, i
        );
    }
}

// ── Spawn retry pattern ────────────────────────────────────────────

/// When a command exits immediately, the window dies and we can detect it.
/// A retry with a working command should create a persistent window.
///
/// Regression test for 5bb8356: fallback to fresh session when --continue fails.
/// The spawn_claude function detects immediate window death (command exits)
/// and retries with a fresh --session-id. This test verifies the underlying
/// tmux building blocks: death detection + successful retry.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_spawn_retry_after_immediate_death() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().to_string();

    // First attempt: command that exits immediately (simulates --continue failing)
    midtown::tmux::create_window(&session, "coworker", &dir, Some("true"))
        .expect("create_window failed");

    // Wait for the window to die
    thread::sleep(Duration::from_millis(500));
    let exists_after_fail =
        midtown::tmux::window_exists(&session, "coworker").expect("window_exists failed");
    assert!(
        !exists_after_fail,
        "Window with immediately-exiting command should die — this is how \
         spawn_claude detects --continue failures"
    );

    // Retry: command that stays alive (simulates fresh --session-id spawn)
    midtown::tmux::create_window(&session, "coworker", &dir, Some("sleep 300"))
        .expect("retry create_window failed");
    thread::sleep(Duration::from_millis(300));

    let exists_after_retry =
        midtown::tmux::window_exists(&session, "coworker").expect("window_exists failed");
    assert!(
        exists_after_retry,
        "Retry with a working command should create a persistent window — \
         this is the fallback path in spawn_claude when --continue fails"
    );
}

// ── Process cleanup on session kill ─────────────────────────────────

/// Pane processes must be terminated when the session is destroyed.
///
/// Regression test for orphaned Claude Code processes: node-based TUI apps
/// install SIGHUP handlers, so tmux kill-session (which sends SIGHUP) leaves
/// them running as orphans. terminate_session_processes sends SIGTERM first.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_terminate_session_processes_kills_pane_processes() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    // No SessionCleanup — we kill the session manually in this test

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().to_string();

    // Create windows with long-running processes (simulating coworkers)
    for name in ["coworker-a", "coworker-b", "coworker-c"] {
        midtown::tmux::create_window(&session, name, &dir, Some("sleep 300"))
            .unwrap_or_else(|e| panic!("create_window({}) failed: {}", name, e));
    }
    thread::sleep(Duration::from_millis(300));

    // Collect pane PIDs before termination
    let pids = midtown::tmux::session_pane_pids(&session);
    assert!(
        pids.len() >= 3,
        "Expected at least 3 panes, got: {:?}",
        pids
    );

    // SIGTERM all processes then kill session
    midtown::tmux::terminate_session_processes(&session);
    kill_test_session(&session);

    // Verify all pane processes are dead
    thread::sleep(Duration::from_millis(500));
    for (name, pid) in &pids {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            !alive,
            "Process {} (pid {}) survived session destruction — \
             orphaned processes consume memory and cause contention",
            name, pid
        );
    }
}

/// Node processes with SIGHUP handlers survive tmux kill-session without
/// explicit SIGTERM. This test proves the problem exists.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_node_sighup_handler_survives_kill_session() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    // This test requires node
    let node_available = std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !node_available {
        eprintln!("node not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().to_string();

    // Create a node process that ignores SIGHUP (simulates Claude Code)
    let script = "process.on('SIGHUP', () => {}); setInterval(() => {}, 1000)";
    let cmd = format!("exec node -e \"{}\"", script);
    midtown::tmux::create_window(&session, "sighup-test", &dir, Some(&cmd))
        .expect("create_window failed");
    thread::sleep(Duration::from_millis(500));

    let pids = midtown::tmux::session_pane_pids(&session);
    let node_pid = pids
        .iter()
        .find(|(name, _)| name.contains("sighup"))
        .map(|(_, pid)| *pid)
        .expect("Couldn't find sighup-test pane");

    // Kill session WITHOUT terminate_session_processes (the old behavior)
    kill_test_session(&session);
    thread::sleep(Duration::from_millis(500));

    let alive = std::process::Command::new("kill")
        .args(["-0", &node_pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    // Clean up the orphan
    let _ = std::process::Command::new("kill")
        .arg(node_pid.to_string())
        .status();

    if !alive {
        eprintln!(
            "Node process did not survive kill-session on this tmux version — \
             orphan problem does not reproduce here. SIGTERM cleanup is still \
             a defensive measure for tmux versions where SIGHUP is insufficient."
        );
        return;
    }
}

/// terminate_session_processes must also kill child processes (descendants).
///
/// Claude Code spawns node subprocesses. If we only kill the direct pane process
/// (the shell), the node children can become orphans. The improved implementation
/// uses pgrep -P to find and kill all descendants.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_terminate_session_processes_kills_child_processes() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().to_string();

    // Create a window with a parent process that spawns a child
    // The parent is "sh -c" and it spawns "sleep 300" as a child
    midtown::tmux::create_window(
        &session,
        "parent-child",
        &dir,
        Some("sh -c 'sleep 300 & wait'"),
    )
    .expect("create_window failed");
    thread::sleep(Duration::from_millis(500));

    // Get the pane PID (the shell)
    let pane_pids = midtown::tmux::session_pane_pids(&session);
    let parent_pid = pane_pids
        .iter()
        .find(|(name, _)| name.contains("parent"))
        .map(|(_, pid)| *pid)
        .expect("Couldn't find parent-child pane");

    // Find child processes using pgrep
    let child_pids_output = Command::new("pgrep")
        .args(["-P", &parent_pid.to_string()])
        .output();

    let child_pids: Vec<u32> = match child_pids_output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect(),
        _ => vec![],
    };

    // Terminate and kill session
    midtown::tmux::terminate_session_processes(&session);
    kill_test_session(&session);

    // Verify parent is dead
    thread::sleep(Duration::from_millis(300));
    let parent_alive = Command::new("kill")
        .args(["-0", &parent_pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(
        !parent_alive,
        "Parent process {} should be dead",
        parent_pid
    );

    // Verify all children are dead
    for child_pid in &child_pids {
        let child_alive = Command::new("kill")
            .args(["-0", &child_pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            !child_alive,
            "Child process {} should be dead — descendant tracking failed",
            child_pid
        );
    }
}

/// Test that stubborn processes (that ignore SIGTERM) are force-killed.
///
/// The improved terminate_session_processes sends SIGTERM, waits up to 2s,
/// then sends SIGKILL to any survivors.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_terminate_session_processes_force_kills_stubborn_processes() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().to_string();

    // Create a process that ignores SIGTERM (trap '' TERM)
    midtown::tmux::create_window(&session, "stubborn", &dir, Some("trap '' TERM; sleep 300"))
        .expect("create_window failed");
    thread::sleep(Duration::from_millis(500));

    let pane_pids = midtown::tmux::session_pane_pids(&session);
    let stubborn_pid = pane_pids
        .iter()
        .find(|(name, _)| name.contains("stubborn"))
        .map(|(_, pid)| *pid)
        .expect("Couldn't find stubborn pane");

    // Terminate and kill session
    midtown::tmux::terminate_session_processes(&session);
    kill_test_session(&session);

    // The stubborn process should be dead (killed with SIGKILL after timeout)
    thread::sleep(Duration::from_millis(300));
    let alive = Command::new("kill")
        .args(["-0", &stubborn_pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(
        !alive,
        "Stubborn process {} should be force-killed with SIGKILL",
        stubborn_pid
    );
}

/// Test that spawning and stopping a real Claude process works correctly.
///
/// This test spawns an actual claude process in a tmux window, then
/// uses terminate_session_processes to kill it and verifies nothing survives.
#[test]
#[ignore]
#[timeout(60_000)]
fn test_spawn_and_stop_claude_kills_all_processes() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    // Check if claude is available
    let claude_available = Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !claude_available {
        eprintln!("claude not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().to_string();

    // Spawn a real claude process
    let config = midtown::tmux::ClaudeLaunchConfig {
        name: "test-claude".to_string(),
        session_mode: midtown::tmux::SessionMode::Fresh,
        task_mode: midtown::tmux::TaskMode::Isolated,
        role: midtown::tmux::CoworkerRole::default(),
        initial_prompt: Some("Say 'ready' and wait.".to_string()),
        additional_dirs: vec![],
        restrict_setting_sources: true,
        pr_number: None,
    };
    let result = midtown::tmux::spawn_claude(&session, &dir, &config);
    assert!(result.is_ok(), "spawn_claude failed: {:?}", result.err());

    // Wait for claude to start
    thread::sleep(Duration::from_secs(5));

    // Get all pane PIDs and their descendants
    let pane_pids = midtown::tmux::session_pane_pids(&session);
    let mut all_pids: Vec<u32> = pane_pids.iter().map(|(_, pid)| *pid).collect();

    // Find all descendant PIDs
    for (_, pid) in &pane_pids {
        let child_output = Command::new("pgrep")
            .args(["-P", &pid.to_string()])
            .output();
        if let Ok(o) = child_output
            && o.status.success()
        {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Ok(child_pid) = line.trim().parse::<u32>() {
                    all_pids.push(child_pid);
                }
            }
        }
    }
    all_pids.sort();
    all_pids.dedup();

    println!("PIDs before terminate: {:?}", all_pids);

    // Terminate and kill session
    midtown::tmux::terminate_session_processes(&session);
    kill_test_session(&session);

    // Give processes time to die
    thread::sleep(Duration::from_secs(1));

    // Verify ALL processes are dead
    let survivors: Vec<u32> = all_pids
        .iter()
        .copied()
        .filter(|pid| {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
        .collect();

    // Clean up any survivors
    for pid in &survivors {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    }

    assert!(
        survivors.is_empty(),
        "Orphaned Claude processes survived: {:?}. \
         midtown stop should kill all processes it started.",
        survivors
    );
}

// ── Channel write + read roundtrip ─────────────────────────────────

/// Messages written to the channel can be read back with correct content.
///
/// Regression coverage for channel-dependent bug fixes (2ef8722, 57fa9c1):
/// the daemon reads channel messages to detect @lead mentions and route
/// nudges. This test verifies the channel roundtrip that underpins those
/// features.
#[test]
fn test_channel_write_read_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let base_dir = temp.path().join("project");

    let channel = midtown::Channel::new(&base_dir).unwrap();

    // Write messages from different senders
    channel
        .send(&midtown::Message::system(
            "⚠️ @lead Orphaned worktrees: amsterdam",
        ))
        .unwrap();
    channel
        .send(&midtown::Message::text("lexington", "working on task #42"))
        .unwrap();
    channel
        .send(&midtown::Message::text(
            "park",
            "@lead need help with merge conflict",
        ))
        .unwrap();

    // Read all messages back
    let messages = channel.read_all().unwrap();
    assert_eq!(messages.len(), 3, "Should have 3 messages");

    // Verify the system message with @lead is preserved
    assert_eq!(messages[0].from, "system");
    assert!(
        messages[0].content.contains("@lead"),
        "System @lead mention must be preserved for daemon routing"
    );

    // Verify coworker @lead mention is preserved
    assert_eq!(messages[2].from, "park");
    assert!(
        messages[2].content.contains("@lead"),
        "Coworker @lead mention must be preserved for daemon routing"
    );
}

/// System messages from SKIP_SENDERS containing @lead should be
/// distinguishable from regular coworker messages — the daemon uses
/// this to decide whether to nudge the lead.
#[test]
fn test_channel_skip_sender_at_lead_detection() {
    // Mirrors the daemon's chat_monitor_loop logic from fix 2ef8722:
    // SKIP_SENDERS messages with @lead should trigger a lead nudge,
    // EXCEPT messages from "user" (already handled in handle_channel_post).
    let skip_senders = ["system", "midtown", "user"];

    let should_nudge_lead = |from: &str, content: &str| -> bool {
        let is_skip_sender = skip_senders.iter().any(|&s| s.eq_ignore_ascii_case(from));
        is_skip_sender
            && !from.eq_ignore_ascii_case("user")
            && content.to_lowercase().contains("@lead")
    };

    // System message with @lead → nudge
    assert!(should_nudge_lead(
        "system",
        "⚠️ @lead Orphaned worktrees: amsterdam"
    ));

    // Midtown daemon message with @lead → nudge
    assert!(should_nudge_lead("midtown", "@lead attention needed"));

    // User message with @lead → NO nudge (handled elsewhere)
    assert!(!should_nudge_lead("user", "@lead can you help?"));

    // System message without @lead → NO nudge
    assert!(!should_nudge_lead("system", "Channel log rotated"));

    // Regular coworker → not a skip_sender, not handled by this path
    assert!(!should_nudge_lead("lexington", "@lead need help"));
}

// ── Blank-pane (zombie) detection ──────────────────────────────────

/// A command that produces no output (`sleep`) has a blank pane:
/// window_exists is true but pane_has_output is false.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_blank_pane_detected_when_command_produces_no_output() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();
    midtown::tmux::create_window(
        &session,
        "zombie",
        &temp.path().to_string_lossy(),
        Some("sleep 300"),
    )
    .expect("create_window failed");

    // Give tmux a moment to set up the pane
    thread::sleep(Duration::from_millis(500));

    let exists = midtown::tmux::window_exists(&session, "zombie").expect("window_exists failed");
    assert!(exists, "Window should exist");

    let target = format!("{}:zombie", session);
    let has_output = midtown::tmux::pane_has_output(&target);
    assert!(
        !has_output,
        "sleep produces no output — pane should be blank (this is the zombie condition)"
    );
}

/// A command that echoes text has a non-blank pane: pane_has_output is true.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_pane_with_output_detected_correctly() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();
    midtown::tmux::create_window(
        &session,
        "healthy",
        &temp.path().to_string_lossy(),
        Some("echo 'Claude Code is running'; sleep 300"),
    )
    .expect("create_window failed");

    // Wait for echo to produce output
    thread::sleep(Duration::from_millis(500));

    let target = format!("{}:healthy", session);
    let has_output = midtown::tmux::pane_has_output(&target);
    assert!(
        has_output,
        "echo produces output — pane should NOT be blank"
    );
}

/// Unit test for content_has_output — no tmux needed.
#[test]
fn test_blank_pane_content_detection() {
    // Completely empty
    assert!(!midtown::tmux::content_has_output(""));

    // Only whitespace and blank lines
    assert!(!midtown::tmux::content_has_output("   \n\n  \n   \n"));

    // Only newlines
    assert!(!midtown::tmux::content_has_output("\n\n\n\n"));

    // Has actual content
    assert!(midtown::tmux::content_has_output("Hello, world!"));

    // Content buried in blank lines
    assert!(midtown::tmux::content_has_output("\n\n  Claude Code  \n\n"));

    // Single non-whitespace character
    assert!(midtown::tmux::content_has_output("\n.\n"));
}

// ── spawn_claude TUI visibility ─────────────────────────────────────

/// Spawning claude with an initial prompt must produce a visible TUI.
///
/// Regression test: build_claude_command previously used `-p` to pass the
/// initial prompt, but `-p` is `--print` mode which disables the interactive
/// TUI and makes the pane appear blank. The prompt must be a bare positional
/// argument so claude launches in interactive mode.
#[test]
#[ignore]
#[timeout(30_000)]
fn test_spawn_claude_with_initial_prompt_renders_tui() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().to_string();

    // Spawn claude with an initial prompt — this is the code path that was
    // broken when -p (--print mode) was used instead of a positional arg.
    let config = midtown::tmux::ClaudeLaunchConfig {
        name: "test-coworker".to_string(),
        session_mode: midtown::tmux::SessionMode::Fresh,
        task_mode: midtown::tmux::TaskMode::Isolated,
        role: midtown::tmux::CoworkerRole::default(),
        initial_prompt: Some("Say hello and wait for instructions.".to_string()),
        additional_dirs: vec![],
        restrict_setting_sources: true,
        pr_number: None,
    };
    let result = midtown::tmux::spawn_claude(&session, &dir, &config);

    assert!(result.is_ok(), "spawn_claude failed: {:?}", result.err());

    // spawn_claude already waits up to 8s for output (3s stability + 5s
    // blank pane check). Give a bit more time for the TUI to render.
    thread::sleep(Duration::from_secs(3));

    let target = format!("{}:test-coworker", session);
    let has_output = midtown::tmux::pane_has_output(&target);
    assert!(
        has_output,
        "spawn_claude with initial_prompt must produce a visible TUI — \
         blank pane means claude launched in --print mode instead of interactive"
    );

    // Clean up: kill the claude process
    kill_window(&session, "test-coworker");
}

/// Test that bell notification only goes to the chat pane (lead.1), not the
/// Claude Code pane (lead.0).
///
/// Background: ASCII 7 (BEL / \x07) is both the terminal bell AND Ctrl+G.
/// When sent via `tmux send-keys -l` to Claude Code, it triggers the "open
/// editor" shortcut instead of producing a notification. We must only send
/// the bell to the chat pane (.1) which handles it correctly.
///
/// This test verifies:
/// 1. send_bell to lead.1 succeeds (chat pane can receive notifications)
/// 2. send_bell to lead.0 is NOT called by notify_user (prevents Ctrl+G trigger)
#[test]
#[ignore]
#[timeout(30_000)]
fn test_send_bell_only_targets_chat_pane() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    // Rename default window to "lead" to simulate the lead window
    let default_window = format!("{}:0", session);
    let _ = Command::new("tmux")
        .args(["rename-window", "-t", &default_window, "lead"])
        .status();
    thread::sleep(Duration::from_millis(100));

    // Split the lead window to create pane .1 (chat pane)
    let lead_target = format!("{}:lead", session);
    let _ = Command::new("tmux")
        .args(["split-window", "-h", "-t", &lead_target])
        .status();
    thread::sleep(Duration::from_millis(100));

    // Verify we have two panes
    assert_eq!(
        pane_count(&session, "lead"),
        2,
        "Lead window should have 2 panes (Claude Code .0 and chat .1)"
    );

    // Verify send_bell to lead.1 succeeds (the correct target)
    let result = midtown::tmux::send_bell(&session, "lead.1");
    assert!(
        result.is_ok(),
        "send_bell to lead.1 (chat pane) should succeed: {:?}",
        result.err()
    );

    // NOTE: We do NOT test send_bell to lead.0 here because the bug is that
    // sending \x07 to Claude Code triggers Ctrl+G. The fix removes that call
    // from notify_user entirely. This test documents the expected behavior:
    // only the chat pane (lead.1) should receive bell notifications.
}

// ── Orphan process cleanup tests ────────────────────────────────────

/// Helper to create an orphaned process that matches a pattern.
///
/// Uses a shell script that forks, has the child exec a long-running process,
/// and the parent exits immediately. This creates a true orphan (PPID=1).
fn spawn_orphan(pattern_arg: &str) -> Option<u32> {
    // Create a script that orphans itself
    // The grandchild runs sh -c with a loop that has the pattern in its command line
    // Parent (subshell) exits immediately, making the grandchild an orphan
    let loop_cmd = format!(
        "while true; do sleep 1; done # orphan-marker {}",
        pattern_arg
    );
    let script = format!(r#"( sh -c '{}' & ) &"#, loop_cmd.replace("'", "'\\''"));

    // Use status() not output() - output() blocks waiting for stdout EOF
    // which can hang when background processes are involved
    let status = Command::new("sh")
        .args(["-c", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;

    if !status.success() {
        return None;
    }

    // Wait for the orphan to be created
    thread::sleep(Duration::from_millis(300));

    // Find the orphan by pattern
    let search_pattern = format!("orphan-marker {}", pattern_arg);
    let pgrep_output = Command::new("pgrep")
        .args(["-f", &search_pattern])
        .output()
        .ok()?;

    String::from_utf8_lossy(&pgrep_output.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
}

/// Helper to spawn a non-orphaned process (has a real parent).
fn spawn_with_parent(pattern_arg: &str) -> Option<std::process::Child> {
    // Use sh -c to run a command with the same pattern as orphan
    // The difference is this one will have a real parent (the test process)
    Command::new("sh")
        .args([
            "-c",
            &format!(
                "while true; do sleep 1; done # orphan-marker {}",
                pattern_arg
            ),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
}

/// `find_orphaned_processes` returns only processes with PPID=1.
///
/// Verifies that the function correctly identifies orphans while ignoring
/// processes that have a legitimate parent.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_find_orphaned_processes_returns_only_orphans() {
    let test_marker = format!("orphan-test-{}", std::process::id());

    // Spawn an orphan with our test marker
    let orphan_pid = spawn_orphan(&test_marker);
    assert!(orphan_pid.is_some(), "Failed to spawn orphan process");
    let orphan_pid = orphan_pid.unwrap();

    // Spawn a non-orphan with the same marker
    let mut child = spawn_with_parent(&test_marker).expect("Failed to spawn child");

    // Give processes time to settle
    thread::sleep(Duration::from_millis(200));

    // Verify the orphan has PPID=1
    let orphan_ppid = midtown::tmux::get_ppid(orphan_pid);
    assert_eq!(
        orphan_ppid,
        Some(1),
        "Orphan should have PPID=1, got {:?}",
        orphan_ppid
    );

    // Verify the non-orphan has a real parent
    let child_ppid = midtown::tmux::get_ppid(child.id());
    assert_ne!(
        child_ppid,
        Some(1),
        "Non-orphan should not have PPID=1, got {:?}",
        child_ppid
    );

    // find_orphaned_processes should only return the orphan
    let pattern = format!("orphan-marker {}", test_marker);
    let orphans = midtown::tmux::find_orphaned_processes(&pattern);

    assert!(
        orphans.contains(&orphan_pid),
        "Orphan PID {} should be in results: {:?}",
        orphan_pid,
        orphans
    );
    assert!(
        !orphans.contains(&child.id()),
        "Non-orphan PID {} should NOT be in results: {:?}",
        child.id(),
        orphans
    );

    // Cleanup
    let _ = Command::new("kill").arg(orphan_pid.to_string()).status();
    let _ = child.kill();
    let _ = child.wait();
}

/// `kill_orphaned_processes` kills only orphaned processes.
///
/// Verifies that:
/// 1. Orphaned processes matching the pattern are killed
/// 2. Non-orphaned processes matching the pattern are NOT killed
#[test]
#[ignore]
#[timeout(15_000)]
fn test_kill_orphaned_processes_kills_only_orphans() {
    let test_marker = format!("kill-orphan-test-{}", std::process::id());

    // Spawn an orphan with our test marker
    let orphan_pid = spawn_orphan(&test_marker);
    assert!(orphan_pid.is_some(), "Failed to spawn orphan process");
    let orphan_pid = orphan_pid.unwrap();

    // Spawn a non-orphan with the same marker
    let mut child = spawn_with_parent(&test_marker).expect("Failed to spawn child");
    let child_pid = child.id();

    // Give processes time to settle
    thread::sleep(Duration::from_millis(200));

    // Verify both are alive before cleanup
    assert!(
        midtown::tmux::is_pid_alive(orphan_pid),
        "Orphan should be alive before cleanup"
    );
    assert!(
        midtown::tmux::is_pid_alive(child_pid),
        "Non-orphan should be alive before cleanup"
    );

    // Kill orphaned processes (only matches orphan-marker, not non-orphan-marker)
    let pattern = format!("orphan-marker {}", test_marker);
    let killed = midtown::tmux::kill_orphaned_processes(&pattern);

    // Should have killed exactly 1 (the orphan)
    assert_eq!(killed, 1, "Should have killed exactly 1 orphan");

    // Give time for processes to die
    thread::sleep(Duration::from_millis(600));

    // Verify orphan is dead
    assert!(
        !midtown::tmux::is_pid_alive(orphan_pid),
        "Orphan PID {} should be dead after cleanup",
        orphan_pid
    );

    // Verify non-orphan is still alive
    assert!(
        midtown::tmux::is_pid_alive(child_pid),
        "Non-orphan PID {} should still be alive after cleanup",
        child_pid
    );

    // Cleanup the non-orphan
    let _ = child.kill();
    let _ = child.wait();
}

/// `kill_orphaned_processes` with midtown settings pattern kills real orphaned Claude.
///
/// This test spawns a real claude process, orphans it by killing the parent session
/// without using terminate_session_processes, then verifies the cleanup function
/// finds and kills it.
#[test]
#[ignore]
#[timeout(60_000)]
fn test_kill_orphaned_claude_processes_real() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    // Check if claude is available
    let claude_available = Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !claude_available {
        eprintln!("claude not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().to_string();

    // Spawn a real claude process
    let config = midtown::tmux::ClaudeLaunchConfig {
        name: "orphan-test".to_string(),
        session_mode: midtown::tmux::SessionMode::Fresh,
        task_mode: midtown::tmux::TaskMode::Isolated,
        role: midtown::tmux::CoworkerRole::default(),
        initial_prompt: Some("Say 'ready' and wait.".to_string()),
        additional_dirs: vec![],
        restrict_setting_sources: true,
        pr_number: None,
    };
    let result = midtown::tmux::spawn_claude(&session, &dir, &config);
    assert!(result.is_ok(), "spawn_claude failed: {:?}", result.err());

    // Wait for claude to start
    thread::sleep(Duration::from_secs(5));

    // Get the claude process PID
    let pids = midtown::tmux::session_pane_pids(&session);
    let claude_pid = pids
        .iter()
        .find(|(name, _)| name.contains("orphan-test"))
        .map(|(_, pid)| *pid)
        .expect("Couldn't find claude pane");

    // Also get child PIDs
    let child_output = Command::new("pgrep")
        .args(["-P", &claude_pid.to_string()])
        .output();
    let mut all_pids = vec![claude_pid];
    if let Ok(o) = child_output
        && o.status.success()
    {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if let Ok(pid) = line.trim().parse::<u32>() {
                all_pids.push(pid);
            }
        }
    }

    println!("Claude PIDs before orphaning: {:?}", all_pids);

    // Kill the tmux session WITHOUT using terminate_session_processes
    // This simulates someone running `tmux kill-session` directly
    kill_test_session(&session);

    // Wait for processes to become orphaned (PPID -> 1)
    thread::sleep(Duration::from_secs(1));

    // Verify at least one process survived and is orphaned
    let survivors: Vec<u32> = all_pids
        .iter()
        .copied()
        .filter(|&pid| midtown::tmux::is_pid_alive(pid))
        .collect();

    println!("Survivors after killing session: {:?}", survivors);

    // Claude should have survived (handles SIGHUP)
    // If no survivors, the test can't verify cleanup
    if survivors.is_empty() {
        eprintln!("No survivors after kill-session, test inconclusive");
        return;
    }

    // Verify at least one survivor is orphaned
    let orphaned_survivors: Vec<u32> = survivors
        .iter()
        .copied()
        .filter(|&pid| midtown::tmux::get_ppid(pid) == Some(1))
        .collect();

    println!("Orphaned survivors: {:?}", orphaned_survivors);

    if orphaned_survivors.is_empty() {
        eprintln!("No orphaned survivors, test inconclusive");
        // Clean up any non-orphaned survivors
        for pid in &survivors {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        return;
    }

    // Now test the cleanup function
    let pattern = "claude.*--settings.*/midtown/.*-settings\\.json";
    let killed = midtown::tmux::kill_orphaned_processes(pattern);

    println!("Killed {} orphaned processes", killed);
    assert!(killed > 0, "Should have killed at least one orphan");

    // Verify all orphans are dead
    thread::sleep(Duration::from_millis(600));
    for pid in &orphaned_survivors {
        assert!(
            !midtown::tmux::is_pid_alive(*pid),
            "Orphan {} should be dead after cleanup",
            pid
        );
    }
}

// ── Send-keys for nudges ────────────────────────────────────────────

/// `send_keys_raw` delivers raw key sequences without Enter.
///
/// Verifies that raw keys can be sent to a pane and captured. This is the
/// foundation for nudge delivery.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_send_keys_raw_delivers_text() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();

    // Create a window running cat which will echo input
    // Use cat -u for unbuffered output
    midtown::tmux::create_window(
        &session,
        "catwin",
        &temp.path().to_string_lossy(),
        Some("cat"),
    )
    .expect("create_window failed");

    thread::sleep(Duration::from_millis(500));

    // Send some text via send_keys_raw (no Enter appended)
    let test_text = "Hello from test";
    midtown::tmux::send_keys_raw(&session, "catwin", test_text).expect("send_keys_raw failed");

    thread::sleep(Duration::from_millis(200));

    // Capture the pane and verify the text appears
    let target = format!("{}:catwin", session);
    let content = midtown::tmux::capture_pane(&target);
    assert!(content.is_some(), "Failed to capture pane");

    // The text should be visible as typed (waiting for Enter)
    let content = content.unwrap();
    assert!(
        content.contains(test_text),
        "Expected '{}' in pane content, got: {}",
        test_text,
        content
    );
}

/// `send_keys_raw` can send special keys like Enter.
///
/// This verifies the mechanism used by nudge delivery to submit input.
/// Uses a marker file to verify the command was actually executed.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_send_keys_raw_enter_submits_input() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();
    let marker_file = temp.path().join("marker.txt");

    // Create a window with a shell
    midtown::tmux::create_window(
        &session,
        "shellwin",
        &temp.path().to_string_lossy(),
        None, // Start a shell
    )
    .expect("create_window failed");

    // Give shell time to start
    thread::sleep(Duration::from_millis(800));

    // Send a touch command followed by Enter - creates a file as proof of execution
    let touch_cmd = format!("touch {}", marker_file.to_string_lossy());
    midtown::tmux::send_keys_raw(&session, "shellwin", &touch_cmd)
        .expect("send_keys_raw text failed");
    thread::sleep(Duration::from_millis(100));
    midtown::tmux::send_keys_raw(&session, "shellwin", "Enter")
        .expect("send_keys_raw Enter failed");

    // Wait for command to execute
    thread::sleep(Duration::from_millis(1000));

    // Verify the marker file was created (proves Enter was sent and command executed)
    assert!(
        marker_file.exists(),
        "Marker file should exist - proves 'Enter' was sent and command executed"
    );
}

// ── Session management ──────────────────────────────────────────────

/// `session_exists` returns true for existing sessions.
#[test]
#[ignore]
#[timeout(15_000)]
#[allow(deprecated)]
fn test_session_exists_returns_true_for_existing() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    // session_exists expects the name WITHOUT the prefix
    // The test session was created with the full name, so we need to extract the suffix
    let session_suffix = session
        .strip_prefix(midtown::tmux::SESSION_PREFIX)
        .unwrap_or(&session);

    let exists = midtown::tmux::session_exists(session_suffix);
    assert!(
        exists.is_ok() && exists.unwrap(),
        "session_exists should return true for existing session"
    );
}

/// `session_exists` returns false for non-existing sessions.
#[test]
#[ignore]
#[timeout(15_000)]
#[allow(deprecated)]
fn test_session_exists_returns_false_for_nonexistent() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let nonexistent = format!("nonexistent-{}", std::process::id());
    let exists = midtown::tmux::session_exists(&nonexistent);
    assert!(
        exists.is_ok() && !exists.unwrap(),
        "session_exists should return false for non-existing session"
    );
}

/// `list_sessions` returns existing midtown sessions.
#[test]
#[ignore]
#[timeout(15_000)]
#[allow(deprecated)]
fn test_list_sessions_finds_midtown_sessions() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    // Create a session with the midtown prefix
    let unique_name = format!("test-list-{}", std::process::id());
    let full_session = format!("{}{}", midtown::tmux::SESSION_PREFIX, unique_name);

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &full_session])
        .status()
        .expect("Failed to create test session");
    assert!(status.success(), "Failed to create midtown test session");

    // Cleanup on drop
    struct MidtownSessionCleanup(String);
    impl Drop for MidtownSessionCleanup {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &self.0])
                .status();
        }
    }
    let _cleanup = MidtownSessionCleanup(full_session);

    let sessions = midtown::tmux::list_sessions().expect("list_sessions failed");

    assert!(
        sessions.contains(&unique_name),
        "list_sessions should find our midtown session '{}', got: {:?}",
        unique_name,
        sessions
    );
}

// ── Status bar hook setup ───────────────────────────────────────────

/// `setup_status_bar_hook` completes without error.
///
/// The hook sets up a pane-focus-in callback that updates the status bar
/// color based on the focused window's name. We verify the function executes
/// successfully - the hook mechanism is internal to tmux.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_setup_status_bar_hook_succeeds() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    // Create a window named after a coworker to ensure the hook has valid targets
    assert!(create_test_window(&session, "lexington"));
    thread::sleep(Duration::from_millis(100));

    // Set up the status bar hook - should succeed
    let result = midtown::tmux::setup_status_bar_hook(&session);
    assert!(result.is_ok(), "setup_status_bar_hook should succeed");

    // Setting the hook again should also succeed (idempotent)
    let result2 = midtown::tmux::setup_status_bar_hook(&session);
    assert!(
        result2.is_ok(),
        "setup_status_bar_hook should be idempotent"
    );
}

/// `setup_status_bar_hook` can be called on sessions with multiple windows.
///
/// Verifies that the hook setup works correctly regardless of how many
/// windows exist in the session.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_setup_status_bar_hook_with_multiple_windows() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    // Create multiple coworker windows
    for name in ["lexington", "park", "madison", "amsterdam"] {
        assert!(
            create_test_window(&session, name),
            "Failed to create {}",
            name
        );
    }
    thread::sleep(Duration::from_millis(100));

    // Set up the status bar hook
    let result = midtown::tmux::setup_status_bar_hook(&session);
    assert!(
        result.is_ok(),
        "setup_status_bar_hook should succeed with multiple windows"
    );

    // Switching windows should not error (even if we can't easily verify the color)
    for name in ["lexington", "park", "madison", "amsterdam"] {
        let target = format!("{}:{}", session, name);
        let status = Command::new("tmux")
            .args(["select-window", "-t", &target])
            .status()
            .expect("select-window failed");
        assert!(status.success(), "Should be able to select window {}", name);
        thread::sleep(Duration::from_millis(50));
    }
}

// ── Window killing safety checks ────────────────────────────────────

/// `kill_window` refuses to kill the last window in a session.
///
/// This safety check prevents accidentally destroying the session (and
/// potentially the tmux server if it's the only session).
#[test]
#[ignore]
#[timeout(15_000)]
fn test_kill_window_refuses_to_kill_last_window() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    // Rename the default window to "only_window"
    let default_target = format!("{}:0", session);
    let _ = Command::new("tmux")
        .args(["rename-window", "-t", &default_target, "only_window"])
        .status();
    thread::sleep(Duration::from_millis(100));

    // Verify there's only one window
    let windows = midtown::tmux::list_all_windows(&session).expect("list_all_windows failed");
    assert_eq!(windows.len(), 1, "Should have exactly one window");

    // Try to kill it - this should NOT work (safety check)
    let result = midtown::tmux::kill_window(&session, "only_window");
    assert!(
        result.is_ok(),
        "kill_window should return Ok (silent failure)"
    );

    // Verify the window still exists
    let still_exists =
        midtown::tmux::window_exists(&session, "only_window").expect("window_exists failed");
    assert!(
        still_exists,
        "Last window should NOT have been killed - safety check failed"
    );
}

/// `kill_window_by_target` also refuses to kill the last window.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_kill_window_by_target_refuses_last_window() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    // Rename the default window
    let default_target = format!("{}:0", session);
    let _ = Command::new("tmux")
        .args(["rename-window", "-t", &default_target, "sole_window"])
        .status();
    thread::sleep(Duration::from_millis(100));

    // Try to kill by target - should refuse
    let target = format!("{}:sole_window", session);
    let result = midtown::tmux::kill_window_by_target(&target);
    assert!(result.is_ok(), "kill_window_by_target should return Ok");

    // Window should still exist
    let still_exists =
        midtown::tmux::window_exists(&session, "sole_window").expect("window_exists failed");
    assert!(
        still_exists,
        "Last window should NOT have been killed via kill_window_by_target"
    );
}

/// `kill_window` works when there are multiple windows.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_kill_window_succeeds_with_multiple_windows() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    // Create an additional window so we have 2
    assert!(create_test_window(&session, "to_kill"));
    thread::sleep(Duration::from_millis(100));

    // Verify we have 2 windows
    let windows = midtown::tmux::list_all_windows(&session).expect("list_all_windows failed");
    assert!(windows.len() >= 2, "Should have at least 2 windows");

    // Now kill should work
    let result = midtown::tmux::kill_window(&session, "to_kill");
    assert!(result.is_ok(), "kill_window should succeed");
    thread::sleep(Duration::from_millis(200));

    // Verify the window is gone
    let still_exists =
        midtown::tmux::window_exists(&session, "to_kill").expect("window_exists failed");
    assert!(!still_exists, "Window 'to_kill' should have been killed");
}

// ── Pane capture and parsing ────────────────────────────────────────

/// `capture_pane` returns content from a tmux pane.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_capture_pane_returns_content() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();

    // Create a window that outputs some text
    midtown::tmux::create_window(
        &session,
        "output",
        &temp.path().to_string_lossy(),
        Some("echo CAPTURE_TEST_MARKER; sleep 300"),
    )
    .expect("create_window failed");

    thread::sleep(Duration::from_millis(500));

    let target = format!("{}:output", session);
    let content = midtown::tmux::capture_pane(&target);

    assert!(content.is_some(), "capture_pane should return Some");
    assert!(
        content.as_ref().unwrap().contains("CAPTURE_TEST_MARKER"),
        "Captured content should contain our marker, got: {}",
        content.unwrap()
    );
}

/// `capture_pane` returns None for non-existent pane.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_capture_pane_returns_none_for_nonexistent() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let target = "nonexistent-session:nonexistent-window";
    let content = midtown::tmux::capture_pane(target);

    assert!(
        content.is_none(),
        "capture_pane should return None for non-existent target"
    );
}

// ── Window resize operations ────────────────────────────────────────

/// `resize_window_width` changes the window width.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_resize_window_width_changes_cols() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    // Create with a specific size
    let _ = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("Failed to create session");
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    // Create a window to resize
    assert!(create_test_window(&session, "resizable"));
    thread::sleep(Duration::from_millis(100));

    // Resize to 120 columns
    let result = midtown::tmux::resize_window_width(&session, "resizable", 120);
    assert!(result.is_ok(), "resize_window_width should succeed");

    // Verify the width changed
    let target = format!("{}:resizable", session);
    let output = Command::new("tmux")
        .args(["display-message", "-t", &target, "-p", "#{window_width}"])
        .output()
        .expect("Failed to get window width");

    let width: u16 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("Failed to parse width");

    assert_eq!(width, 120, "Window width should be 120, got {}", width);
}

/// `resize_window_width` enforces minimum width of 80 columns.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_resize_window_width_enforces_minimum() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    let _ = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("Failed to create session");
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    assert!(create_test_window(&session, "narrow"));
    thread::sleep(Duration::from_millis(100));

    // Try to resize to 40 (below minimum)
    let result = midtown::tmux::resize_window_width(&session, "narrow", 40);
    assert!(result.is_ok(), "resize_window_width should succeed");

    // Verify width is at minimum (80), not 40
    let target = format!("{}:narrow", session);
    let output = Command::new("tmux")
        .args(["display-message", "-t", &target, "-p", "#{window_width}"])
        .output()
        .expect("Failed to get window width");

    let width: u16 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("Failed to parse width");

    assert_eq!(
        width,
        midtown::tmux::MIN_RESIZE_COLS,
        "Window width should be enforced to minimum {}, got {}",
        midtown::tmux::MIN_RESIZE_COLS,
        width
    );
}

/// `reset_window_size` resets to automatic sizing.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_reset_window_size_succeeds() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    let _ = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("Failed to create session");
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    assert!(create_test_window(&session, "autoreset"));
    thread::sleep(Duration::from_millis(100));

    // First resize to a specific width
    midtown::tmux::resize_window_width(&session, "autoreset", 150)
        .expect("resize_window_width failed");

    // Now reset to automatic sizing
    let result = midtown::tmux::reset_window_size(&session, "autoreset");
    assert!(result.is_ok(), "reset_window_size should succeed");
}

// ── count_windows_by_name / kill_all_windows_by_name ────────────────

/// `count_windows_by_name` counts windows correctly.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_count_windows_by_name() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    // Create multiple windows with same base name (simulating duplicates)
    // Note: tmux allows duplicate names, and midtown handles this
    assert!(create_test_window(&session, "dupwin"));
    thread::sleep(Duration::from_millis(100));

    // Create another window with a different name
    assert!(create_test_window(&session, "other"));
    thread::sleep(Duration::from_millis(100));

    let (count, ids) = midtown::tmux::count_windows_by_name(&session, "dupwin")
        .expect("count_windows_by_name failed");

    assert_eq!(count, 1, "Should have 1 window named 'dupwin'");
    assert_eq!(ids.len(), 1, "Should return 1 window ID");

    let (other_count, _) = midtown::tmux::count_windows_by_name(&session, "other")
        .expect("count_windows_by_name failed");
    assert_eq!(other_count, 1, "Should have 1 window named 'other'");

    let (none_count, _) = midtown::tmux::count_windows_by_name(&session, "nonexistent")
        .expect("count_windows_by_name failed");
    assert_eq!(none_count, 0, "Should have 0 windows named 'nonexistent'");
}

/// `kill_all_windows_by_name` kills all windows with the given name.
#[test]
#[ignore]
#[timeout(15_000)]
fn test_kill_all_windows_by_name_removes_duplicates() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    // Create a 'keeper' window first (so we have >1 window for safety check)
    assert!(create_test_window(&session, "keeper"));
    thread::sleep(Duration::from_millis(100));

    // Create a window to kill
    assert!(create_test_window(&session, "victim"));
    thread::sleep(Duration::from_millis(100));

    // Verify 'victim' exists
    assert!(
        midtown::tmux::window_exists(&session, "victim").unwrap(),
        "victim window should exist"
    );

    // Kill all windows named 'victim'
    let killed = midtown::tmux::kill_all_windows_by_name(&session, "victim")
        .expect("kill_all_windows_by_name failed");

    assert_eq!(killed, 1, "Should have killed 1 window");
    thread::sleep(Duration::from_millis(300));

    // Verify 'victim' is gone
    assert!(
        !midtown::tmux::window_exists(&session, "victim").unwrap(),
        "victim window should be gone"
    );

    // 'keeper' should still exist
    assert!(
        midtown::tmux::window_exists(&session, "keeper").unwrap(),
        "keeper window should still exist"
    );
}

// ── Reviewer window naming ──────────────────────────────────────────

/// Spawning a reviewer sets the tmux window name to "name:review#PR".
///
/// Verifies that `spawn_claude` with `ClaudeLaunchConfig::reviewer()` correctly
/// renames the tmux window to include "review#PR" format, distinguishing
/// reviewer coworkers from development coworkers (which use "dev#N").
#[test]
#[ignore]
#[timeout(60_000)]
fn test_spawn_reviewer_sets_window_name_to_review_format() {
    if !tmux_available() {
        eprintln!("tmux not available, skipping");
        return;
    }

    // Check if claude is available
    let claude_available = Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !claude_available {
        eprintln!("claude not available, skipping");
        return;
    }

    let session = test_session_name();
    assert!(create_test_session(&session));
    let _cleanup = SessionCleanup {
        session: session.clone(),
    };

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().to_string();

    // Spawn a reviewer for PR #42
    let config = midtown::tmux::ClaudeLaunchConfig::reviewer("test-reviewer", 42);
    let result = midtown::tmux::spawn_claude(&session, &dir, &config);
    assert!(result.is_ok(), "spawn_claude failed: {:?}", result.err());

    // Wait for the window to be renamed
    thread::sleep(Duration::from_secs(3));

    // Get the raw window names from tmux
    let output = Command::new("tmux")
        .args(["list-windows", "-t", &session, "-F", "#{window_name}"])
        .output()
        .expect("Failed to list windows");

    let raw_names: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect();

    // Verify the window name contains "review#42"
    let has_reviewer_format = raw_names.iter().any(|name| name.contains("review#42"));

    assert!(
        has_reviewer_format,
        "Expected a window with 'review#42' in its name, got: {:?}. \
         Reviewer coworkers should have 'name:review#PR' window format.",
        raw_names
    );

    // Clean up: kill the claude process
    kill_window(&session, "test-reviewer");
}
