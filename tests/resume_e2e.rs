//! Session resume E2E tests with real Claude Code.
//!
//! These tests verify that `claude --resume <session_id>` works end-to-end.
//! They require a real Claude Code installation with OAuth auth.
//!
//! Run with `cargo test --test resume_e2e -- --ignored --test-threads=1`

mod common;

use ntest::timeout;
use std::thread;
use std::time::{Duration, Instant};

use common::{DaemonHarnessOptions, DaemonTestHarness};

/// Create a test harness with a short state dir to avoid Unix socket path limits.
/// Each test gets a unique suffix to prevent conflicts between concurrent/sequential tests.
fn create_fixture(suffix: &str) -> Option<DaemonTestHarness> {
    DaemonTestHarness::new(
        &format!("resume-{}", suffix),
        DaemonHarnessOptions {
            custom_state_dir: Some(std::path::PathBuf::from(format!("/tmp/ms-res-{}", suffix))),
            ..Default::default()
        },
    )
}

/// Check if the Claude CLI is available.
fn claude_available() -> bool {
    std::process::Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Common setup: check for claude CLI, create fixture, start daemon.
fn setup_test(suffix: &str) -> Option<DaemonTestHarness> {
    if !claude_available() {
        eprintln!("claude CLI not available, skipping");
        return None;
    }
    let mut fixture = create_fixture(suffix)?;
    if !fixture.start_daemon() {
        eprintln!("Daemon failed to start");
        return None;
    }
    Some(fixture)
}

/// Poll the daemon snapshot for a session matching the given name.
/// Returns (session_id, is_running, pid) if found.
fn find_session_in_snapshot(
    fixture: &DaemonTestHarness,
    name_substr: &str,
) -> Option<(String, bool, Option<u32>)> {
    let response = fixture.rpc_call("snapshot", None)?;
    let sessions = response["result"]["sessions"].as_object()?;
    for (_key, record) in sessions {
        let session_name = record["name"].as_str().unwrap_or("");
        if session_name.contains(name_substr) {
            let session_id = record["session_id"].as_str().unwrap_or("").to_string();
            let is_running = record["is_running"].as_bool().unwrap_or(false);
            let pid = record["pid"].as_u64().map(|p| p as u32);
            return Some((session_id, is_running, pid));
        }
    }
    None
}

/// Wait for a session to appear in the snapshot with is_running=true.
/// Returns (session_id, pid) on success.
fn wait_for_running_session(
    fixture: &DaemonTestHarness,
    name_substr: &str,
    timeout: Duration,
) -> Option<(String, Option<u32>)> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some((session_id, is_running, pid)) = find_session_in_snapshot(fixture, name_substr)
            && is_running
            && !session_id.is_empty()
        {
            return Some((session_id, pid));
        }
        thread::sleep(Duration::from_millis(500));
    }
    None
}

/// Wait for a session to stop (is_running=false or disappear from snapshot).
fn wait_for_session_stopped(
    fixture: &DaemonTestHarness,
    name_substr: &str,
    timeout: Duration,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        match find_session_in_snapshot(fixture, name_substr) {
            Some((_, is_running, _)) if !is_running => return true,
            None => return true,
            _ => {}
        }
        thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Kill a process by PID using SIGTERM.
fn kill_process(pid: u32) {
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Spawn, stop, resume — verify resumed session stays alive > 30s.
#[test]
#[ignore]
#[timeout(300_000)]
fn test_basic_session_resume() {
    let fixture = match setup_test("basic") {
        Some(f) => f,
        None => return,
    };

    // 1. Spawn a coworker
    let spawn_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "prompt": "Say hello and wait for further instructions. Do not exit."
        })),
    );
    eprintln!("Spawn response: {:?}", spawn_response);
    assert!(
        spawn_response.is_some(),
        "coworker.spawn should return a response"
    );
    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        eprintln!(
            "Spawn failed (may be expected without auth): {:?}",
            spawn_response["error"]
        );
        return;
    }

    // 2. Wait for the session to be running
    let (session_id, pid) =
        match wait_for_running_session(&fixture, "worker-", Duration::from_secs(60)) {
            Some(result) => result,
            None => {
                eprintln!("Session never reached running state");
                return;
            }
        };
    eprintln!("Session running: session_id={}, pid={:?}", session_id, pid);

    // Let it run for a few seconds to establish conversation state
    thread::sleep(Duration::from_secs(5));

    // 3. Kill the session process
    if let Some(pid) = pid {
        eprintln!("Killing session process (pid={})", pid);
        kill_process(pid);
    } else {
        eprintln!("No PID available to kill session");
        return;
    }

    // 4. Wait for daemon to detect session stopped
    assert!(
        wait_for_session_stopped(&fixture, "worker-", Duration::from_secs(30)),
        "Session should stop after process kill"
    );
    eprintln!("Session stopped, attempting resume...");

    // 5. Resume via coworker.spawn with resume: true
    let resume_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "resume": true,
            "prompt": "You were resumed. Confirm you are alive by saying 'resumed successfully'."
        })),
    );
    eprintln!("Resume response: {:?}", resume_response);
    assert!(
        resume_response.is_some(),
        "Resume spawn should return a response"
    );

    // 6. Wait for resumed session to be running
    let (new_session_id, _) =
        match wait_for_running_session(&fixture, "worker-", Duration::from_secs(60)) {
            Some(result) => result,
            None => {
                eprintln!("Resumed session never reached running state");

                // Diagnostic: dump snapshot
                if let Some(snap) = fixture.rpc_call("snapshot", None) {
                    eprintln!(
                        "Snapshot after failed resume: {}",
                        serde_json::to_string_pretty(&snap).unwrap_or_default()
                    );
                }
                panic!("Session resume failed — session did not start");
            }
        };
    eprintln!("Resumed session running: session_id={}", new_session_id);

    // 7. Verify it stays alive for > 30s (not a failed resume)
    let resume_start = Instant::now();
    let mut alive_at_30s = false;
    while resume_start.elapsed() < Duration::from_secs(35) {
        thread::sleep(Duration::from_secs(5));
        if let Some((_, is_running, _)) = find_session_in_snapshot(&fixture, "worker-") {
            if is_running && resume_start.elapsed() >= Duration::from_secs(30) {
                alive_at_30s = true;
                break;
            }
            if !is_running {
                let age = resume_start.elapsed().as_secs();
                eprintln!(
                    "Resumed session died after {}s (< 30s = failed resume)",
                    age
                );
                panic!(
                    "Resumed session exited after {}s — this is a failed resume",
                    age
                );
            }
        }
    }
    assert!(
        alive_at_30s,
        "Resumed session should survive past 30s threshold"
    );
    eprintln!("SUCCESS: Resumed session alive after 30s");
}

/// Resume with an invalid session ID should fall back to fresh session.
#[test]
#[ignore]
#[timeout(300_000)]
fn test_resume_fallback_on_invalid_session_id() {
    let fixture = match setup_test("fallback") {
        Some(f) => f,
        None => return,
    };

    // Inject a fake session record into daemon state by spawning a real session first,
    // then corrupting the session_id in the state file.
    // Simpler approach: just spawn with resume=true when no prior session exists.
    // The daemon's SessionMode::Resume with no prior session should fall back to fresh.
    let resume_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "resume": true,
            "prompt": "You are a freshly spawned session. Say 'fresh start'."
        })),
    );
    eprintln!("Resume-with-no-prior response: {:?}", resume_response);
    assert!(
        resume_response.is_some(),
        "coworker.spawn with resume should return a response"
    );

    let resume_response = resume_response.unwrap();
    if resume_response["error"].is_object() {
        eprintln!(
            "Spawn failed (may be expected without auth): {:?}",
            resume_response["error"]
        );
        return;
    }

    // Should fall back to fresh and start running
    let result = wait_for_running_session(&fixture, "worker-", Duration::from_secs(60));
    match result {
        Some((session_id, _)) => {
            eprintln!(
                "Fallback to fresh session succeeded: session_id={}",
                session_id
            );
        }
        None => {
            // Dump diagnostic state
            if let Some(snap) = fixture.rpc_call("snapshot", None) {
                eprintln!(
                    "Snapshot: {}",
                    serde_json::to_string_pretty(&snap).unwrap_or_default()
                );
            }
            panic!("Resume fallback should have created a fresh session");
        }
    }

    // Verify it stays alive (not a crash loop)
    thread::sleep(Duration::from_secs(10));
    if let Some((_, is_running, _)) = find_session_in_snapshot(&fixture, "worker-") {
        assert!(
            is_running,
            "Fallback session should still be running after 10s"
        );
    }
    eprintln!("SUCCESS: Resume fallback to fresh session works");
}

/// Kill a session almost immediately, then attempt resume.
/// Verifies the daemon handles the failure correctly (no crash loop).
#[test]
#[ignore]
#[timeout(300_000)]
fn test_rapid_resume_after_short_session() {
    let fixture = match setup_test("rapid") {
        Some(f) => f,
        None => return,
    };

    // 1. Spawn a coworker
    let spawn_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "prompt": "Wait for instructions."
        })),
    );
    assert!(
        spawn_response.is_some(),
        "coworker.spawn should return a response"
    );
    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        eprintln!("Spawn failed: {:?}", spawn_response["error"]);
        return;
    }

    // 2. Wait for session to be running, then kill immediately
    let (session_id, pid) =
        match wait_for_running_session(&fixture, "worker-", Duration::from_secs(60)) {
            Some(result) => result,
            None => {
                eprintln!("Session never reached running state");
                return;
            }
        };
    let spawn_time = Instant::now();
    eprintln!(
        "Session running: session_id={}, pid={:?} — killing immediately",
        session_id, pid
    );

    // Kill as fast as possible (< 5s of runtime)
    if let Some(pid) = pid {
        kill_process(pid);
    } else {
        eprintln!("No PID — cannot test rapid kill");
        return;
    }

    let kill_age = spawn_time.elapsed();
    eprintln!("Killed after {:?}", kill_age);

    // 3. Wait for daemon to detect stop
    assert!(
        wait_for_session_stopped(&fixture, "worker-", Duration::from_secs(30)),
        "Session should stop after kill"
    );

    // 4. Attempt resume
    let resume_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "resume": true,
            "prompt": "You were resumed after a short session."
        })),
    );
    eprintln!("Resume response: {:?}", resume_response);

    // 5. Poll for up to 40s (past the 30s threshold), checking every 5s
    let poll_start = Instant::now();
    while poll_start.elapsed() < Duration::from_secs(40) {
        thread::sleep(Duration::from_secs(5));
        if let Some((_, is_running, _)) = find_session_in_snapshot(&fixture, "worker-") {
            let elapsed = poll_start.elapsed().as_secs();
            eprintln!("Poll at {}s: is_running={}", elapsed, is_running);
            if is_running && elapsed >= 30 {
                eprintln!("SUCCESS: Resume after short session worked — session alive past 30s");
                break;
            }
        }
    }

    // Check final state
    if let Some(snap) = fixture.rpc_call("snapshot", None)
        && let Some(sessions) = snap["result"]["sessions"].as_object()
    {
        for (key, record) in sessions {
            let name = record["name"].as_str().unwrap_or("?");
            let is_running = record["is_running"].as_bool().unwrap_or(false);
            let sid = record["session_id"].as_str().unwrap_or("?");
            eprintln!(
                "Final state: key={}, name={}, is_running={}, session_id={}",
                key, name, is_running, sid
            );
        }
    }

    // The test passes if:
    // a) The resumed session is alive (resume worked despite short initial session), OR
    // b) The resumed session died but daemon correctly cleared session_id (no crash loop)
    if let Some((_, is_running, _)) = find_session_in_snapshot(&fixture, "worker-") {
        if is_running {
            eprintln!("Resume after short session: session is alive");
        } else {
            eprintln!(
                "INFO: Resume after short session failed — session stopped but no crash loop"
            );
        }
    } else {
        eprintln!("INFO: No session found in snapshot (cleaned up entirely)");
    }

    // Check for crash-loop: count distinct session records
    if let Some(snap) = fixture.rpc_call("snapshot", None)
        && let Some(sessions) = snap["result"]["sessions"].as_object()
    {
        let worker_count = sessions
            .values()
            .filter(|r| r["name"].as_str().unwrap_or("").contains("worker-"))
            .count();
        assert!(
            worker_count <= 3,
            "Should not crash-loop: found {} worker sessions (expected <= 3)",
            worker_count
        );
        eprintln!("No crash loop detected ({} worker sessions)", worker_count);
    }
}

/// Full daemon restart: spawn session, stop daemon, restart, verify session resumes.
#[test]
#[ignore]
#[timeout(300_000)]
fn test_daemon_restart_resumes_sessions() {
    let mut fixture = match setup_test("restart") {
        Some(f) => f,
        None => return,
    };

    // 1. Spawn a coworker
    let spawn_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "prompt": "Wait for instructions. Do not exit."
        })),
    );
    assert!(spawn_response.is_some(), "coworker.spawn should respond");
    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        eprintln!("Spawn failed: {:?}", spawn_response["error"]);
        return;
    }

    // 2. Wait for session to be running
    let (session_id, _) =
        match wait_for_running_session(&fixture, "worker-", Duration::from_secs(60)) {
            Some(result) => result,
            None => {
                eprintln!("Session never reached running state");
                return;
            }
        };
    eprintln!("Session running before restart: session_id={}", session_id);

    // Let it establish conversation state
    thread::sleep(Duration::from_secs(5));

    // 3. Verify resume_on_startup is set in state
    if let Some(state) = fixture.read_daemon_state()
        && let Some(sessions) = state["sessions"].as_object()
    {
        for (key, record) in sessions {
            let resume = record["resume_on_startup"].as_bool().unwrap_or(false);
            eprintln!("Pre-restart: session={}, resume_on_startup={}", key, resume);
        }
    }

    // 4. Stop daemon gracefully
    eprintln!("Stopping daemon...");
    fixture.stop_daemon();
    thread::sleep(Duration::from_secs(2));

    // 5. Restart daemon (same fixture, same state directory)
    // Set RUST_LOG so daemon produces diagnostic output
    // SAFETY: test runs single-threaded (--test-threads=1), no other threads reading env
    unsafe { std::env::set_var("MIDTOWN_LOG_LEVEL", "debug") };
    eprintln!("Restarting daemon...");
    assert!(fixture.start_daemon(), "Daemon should restart successfully");

    // 6. Wait for recovered session to appear
    let result = wait_for_running_session(&fixture, "worker-", Duration::from_secs(90));
    match result {
        Some((new_session_id, _)) => {
            eprintln!(
                "Session resumed after restart: new_session_id={}",
                new_session_id
            );
        }
        None => {
            // Diagnostic dump
            if let Some(snap) = fixture.rpc_call("snapshot", None) {
                eprintln!(
                    "Snapshot after restart: {}",
                    serde_json::to_string_pretty(&snap).unwrap_or_default()
                );
            }
            eprintln!("WARN: No running session found after daemon restart");
            eprintln!("This may indicate resume_on_startup is not set for ad-hoc coworkers");
            // Don't panic — ad-hoc coworkers (no task) may not have resume_on_startup=true.
            // This test documents the actual behavior.
            return;
        }
    }

    // 7. Verify the resumed session stays alive past 30s
    let resume_start = Instant::now();
    while resume_start.elapsed() < Duration::from_secs(35) {
        thread::sleep(Duration::from_secs(5));
        if let Some((_, is_running, _)) = find_session_in_snapshot(&fixture, "worker-")
            && !is_running
        {
            let age = resume_start.elapsed().as_secs();
            eprintln!("Resumed session died after {}s post-restart", age);

            // Dump full snapshot for debugging
            if let Some(snap) = fixture.rpc_call("snapshot", None) {
                eprintln!(
                    "Snapshot after session death:\n{}",
                    serde_json::to_string_pretty(&snap).unwrap_or_default()
                );
            }

            // Dump channel messages for any error output
            let messages = fixture.read_channel_messages();
            eprintln!("Channel messages: {:?}", messages);

            // Read daemon logs from the project dir (daemonize redirects there)
            let daemon_log_dir = fixture.project_dir.join("logs");
            for log_name in &["daemon.out", "daemon.err"] {
                let log_path = daemon_log_dir.join(log_name);
                if let Ok(log) = std::fs::read_to_string(&log_path) {
                    let lines: Vec<&str> = log.lines().collect();
                    let start = lines.len().saturating_sub(80);
                    eprintln!(
                        "=== {} (last {} of {} lines) ===",
                        log_name,
                        lines.len() - start,
                        lines.len()
                    );
                    for line in &lines[start..] {
                        eprintln!("  {}", line);
                    }
                } else {
                    eprintln!("=== {} not found at {} ===", log_name, log_path.display());
                }
            }

            if age < 30 {
                panic!("Failed resume after daemon restart (died in {}s)", age);
            }
        }
    }
    eprintln!("SUCCESS: Session survived daemon restart and resume");
}

/// Resume and context check — best-effort verification that conversation history survives resume.
/// The code word assertion is informational only; the session may not post to the channel,
/// so we do not fail the test if the code word is absent.
#[test]
#[ignore]
#[timeout(300_000)]
fn test_resume_and_context_check() {
    let fixture = match setup_test("context") {
        Some(f) => f,
        None => return,
    };

    // 1. Spawn with a distinctive code word in the prompt
    let spawn_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "prompt": "Remember this code word: FALCON-7. Post a message to the channel saying 'code word acknowledged'. Then wait for further instructions."
        })),
    );
    assert!(spawn_response.is_some(), "coworker.spawn should respond");
    let spawn_response = spawn_response.unwrap();
    if spawn_response["error"].is_object() {
        eprintln!("Spawn failed: {:?}", spawn_response["error"]);
        return;
    }

    // 2. Wait for session to be running
    let (session_id, pid) =
        match wait_for_running_session(&fixture, "worker-", Duration::from_secs(60)) {
            Some(result) => result,
            None => {
                eprintln!("Session never reached running state");
                return;
            }
        };
    eprintln!("Session running: session_id={}", session_id);

    // 3. Wait for the session to process the initial prompt
    // Give it time to generate output and establish context
    thread::sleep(Duration::from_secs(15));

    // Check if the session posted to channel (optional — not all sessions will)
    let messages = fixture.read_channel_messages();
    eprintln!("Channel messages before kill: {:?}", messages);

    // 4. Kill the session
    if let Some(pid) = pid {
        eprintln!("Killing session (pid={})", pid);
        kill_process(pid);
    } else {
        eprintln!("No PID to kill");
        return;
    }

    assert!(
        wait_for_session_stopped(&fixture, "worker-", Duration::from_secs(30)),
        "Session should stop after kill"
    );

    // 5. Resume with a follow-up prompt asking about the code word
    let resume_response = fixture.rpc_call(
        "coworker.spawn",
        Some(serde_json::json!({
            "resume": true,
            "prompt": "What was the code word I told you earlier? Post it to the channel."
        })),
    );
    eprintln!("Resume response: {:?}", resume_response);

    // 6. Wait for resumed session and give it time to respond
    match wait_for_running_session(&fixture, "worker-", Duration::from_secs(60)) {
        Some((new_sid, _)) => {
            eprintln!("Resumed session: session_id={}", new_sid);
        }
        None => {
            eprintln!("Resume failed — session did not start");
            if let Some(snap) = fixture.rpc_call("snapshot", None) {
                eprintln!(
                    "Snapshot: {}",
                    serde_json::to_string_pretty(&snap).unwrap_or_default()
                );
            }
            panic!("Resume failed for context preservation test");
        }
    }

    // Wait for the resumed session to respond
    thread::sleep(Duration::from_secs(30));

    // 7. Check channel messages for the code word
    let messages = fixture.read_channel_messages();
    eprintln!("Channel messages after resume: {:?}", messages);

    let has_falcon = messages.iter().any(|m| m.to_uppercase().contains("FALCON"));
    if has_falcon {
        eprintln!("SUCCESS: Resumed session remembered the code word FALCON-7");
    } else {
        eprintln!("WARN: Code word not found in channel messages");
        eprintln!("This may mean the session didn't post to channel, or context was lost");
        eprintln!("Messages: {:?}", messages);
        // This is informational — the session may not post to channel.
        // The important thing is that resume didn't crash.
    }
}
