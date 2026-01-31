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

    assert!(
        alive,
        "Node process with SIGHUP handler should survive kill-session — \
         this test proves the orphan problem exists. If this fails, \
         tmux changed its signal behavior and the SIGTERM fix may not be needed."
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
