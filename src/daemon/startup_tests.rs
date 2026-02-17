use super::*;

use chrono::Utc;

/// Create a test HeadlessSessionInfo.
fn test_session_info(
    name: &str,
    task_id: Option<u64>,
) -> crate::daemon::state::HeadlessSessionInfo {
    crate::daemon::state::HeadlessSessionInfo {
        session_id: format!("session-{}", name),
        last_active: Utc::now(),
        purpose: format!("test session for {}", name),
        pid: Some(99999), // Non-existent PID
        coworker_type: Some("dev".to_string()),
        task_id,
        pr_number: None,
        working_dir: Some("/tmp/test".to_string()),
        provider: None,
        profile: None,
        resume_on_startup: true,
    }
}

#[tokio::test]
async fn test_recover_headless_sessions_generates_resume_effects() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    // Insert test sessions
    {
        let mut state = persistent_state.lock().await;
        state.headless_sessions.insert(
            "amsterdam".to_string(),
            test_session_info("amsterdam", Some(42)),
        );
        state.headless_sessions.insert(
            "columbus".to_string(),
            test_session_info("columbus", Some(43)),
        );
    }

    let effects = recover_headless_sessions(&persistent_state, "test-repo").await;

    // Should generate exactly 2 ResumeCoworker effects (one per session)
    assert_eq!(
        effects.len(),
        2,
        "Should generate one ResumeCoworker per session"
    );

    for effect in &effects {
        match effect {
            Effect::ResumeCoworker {
                name, session_id, ..
            } => {
                assert!(
                    name == "amsterdam" || name == "columbus",
                    "Unexpected coworker name: {}",
                    name
                );
                assert!(
                    session_id.starts_with("session-"),
                    "Session ID should match what was persisted"
                );
            }
            _ => panic!("Expected only ResumeCoworker effects, got {:?}", effect),
        }
    }
}

#[tokio::test]
async fn test_recover_headless_sessions_does_not_kill_processes() {
    // This test verifies that recover_headless_sessions does NOT generate
    // any kill effects. The old behavior was to kill -9 the processes,
    // which defeated the purpose of session detachment.
    //
    // We verify this by checking that only ResumeCoworker effects are returned.
    // If kill behavior were added back, it would need to be an Effect variant,
    // and this test would catch the regression.
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        state
            .headless_sessions
            .insert("park".to_string(), test_session_info("park", Some(100)));
    }

    let effects = recover_headless_sessions(&persistent_state, "test-repo").await;

    // All effects should be ResumeCoworker — no kill effects
    for effect in &effects {
        assert!(
            matches!(effect, Effect::ResumeCoworker { .. }),
            "Recovery should only produce ResumeCoworker effects (no kills), got: {:?}",
            effect
        );
    }
}

#[tokio::test]
async fn test_recover_headless_sessions_empty() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    let effects = recover_headless_sessions(&persistent_state, "test-repo").await;
    assert!(
        effects.is_empty(),
        "No sessions to recover should produce no effects"
    );
}

#[tokio::test]
async fn test_recovering_coworker_names_returns_session_names() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        state.headless_sessions.insert(
            "amsterdam".to_string(),
            test_session_info("amsterdam", Some(42)),
        );
        state.headless_sessions.insert(
            "columbus".to_string(),
            test_session_info("columbus", Some(43)),
        );
    }

    let names = recovering_coworker_names(&persistent_state).await;
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"amsterdam".to_string()));
    assert!(names.contains(&"columbus".to_string()));
}

#[tokio::test]
async fn test_recover_headless_sessions_skips_non_resumable_historical_entries() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        state.headless_sessions.insert(
            "amsterdam".to_string(),
            test_session_info("amsterdam", Some(42)),
        );
        let mut historical = test_session_info("columbus", Some(43));
        historical.resume_on_startup = false;
        state
            .headless_sessions
            .insert("columbus".to_string(), historical);
    }

    let effects = recover_headless_sessions(&persistent_state, "test-repo").await;
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::ResumeCoworker { name, .. } => assert_eq!(name, "amsterdam"),
        other => panic!("Expected ResumeCoworker, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recovering_coworker_names_skips_non_resumable_historical_entries() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        state.headless_sessions.insert(
            "amsterdam".to_string(),
            test_session_info("amsterdam", Some(42)),
        );
        let mut historical = test_session_info("columbus", Some(43));
        historical.resume_on_startup = false;
        state
            .headless_sessions
            .insert("columbus".to_string(), historical);
    }

    let names = recovering_coworker_names(&persistent_state).await;
    assert_eq!(names, vec!["amsterdam".to_string()]);
}

#[tokio::test]
async fn test_recovering_coworker_names_empty_state() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    let names = recovering_coworker_names(&persistent_state).await;
    assert!(names.is_empty());
}

#[tokio::test]
async fn test_startup_recovery_sets_lead_role() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        state.headless_sessions.insert(
            "lead".to_string(),
            crate::daemon::state::HeadlessSessionInfo {
                session_id: "session-lead".to_string(),
                last_active: chrono::Utc::now(),
                purpose: "lead session".to_string(),
                pid: Some(99999),
                coworker_type: None,
                task_id: None,
                pr_number: None,
                working_dir: Some("/tmp/test".to_string()),
                provider: None,
                profile: None,
                resume_on_startup: true,
            },
        );
    }

    let effects = recover_headless_sessions(&persistent_state, "test-repo").await;
    assert_eq!(effects.len(), 1);

    match &effects[0] {
        Effect::ResumeCoworker { name, config, .. } => {
            assert_eq!(name, "lead");
            assert_eq!(config.role, crate::launch::CoworkerRole::Lead);
            // Setting sources are now always "project,local" via the platform arg builder
        }
        other => panic!("Expected ResumeCoworker, got {:?}", other),
    }
}

// --- Tests for stale daemon and zombie scanner helpers ---

#[test]
fn test_is_stale_midtown_daemon_excludes_current_pid() {
    // The current daemon PID should never be considered stale
    let current_pid = std::process::id();
    assert!(
        !is_stale_midtown_daemon(current_pid, current_pid),
        "Current daemon PID should not be considered stale"
    );
}

#[test]
fn test_is_stale_midtown_daemon_returns_false_for_nonexistent_pid() {
    // A non-existent PID should not be considered a stale daemon
    let fake_pid = 99999;
    let current_pid = std::process::id();
    assert!(
        !is_stale_midtown_daemon(fake_pid, current_pid),
        "Non-existent PID should not be considered a stale midtown daemon"
    );
}

#[test]
fn test_verify_midtown_process_returns_false_for_nonexistent_pid() {
    assert!(
        !verify_midtown_process(99999),
        "Non-existent PID should not verify as midtown"
    );
}

#[test]
fn test_verify_midtown_process_returns_false_for_non_midtown_process() {
    // PID 1 (launchd/init) is definitely not a midtown process
    assert!(
        !verify_midtown_process(1),
        "PID 1 (init/launchd) should not verify as midtown"
    );
}

#[test]
fn test_kill_stale_daemon_skips_non_midtown_process() {
    // PID 1 (launchd/init) should be skipped because it's not a midtown process.
    // This test verifies that kill_stale_daemon doesn't attempt to kill
    // non-midtown processes (it just logs and returns).
    // If it incorrectly tried to kill PID 1, the test environment would error.
    kill_stale_daemon(1);
    // If we get here without panic/error, the function correctly skipped PID 1
}

#[test]
fn test_kill_stale_daemon_skips_own_pid() {
    // Should be a no-op when called with our own PID
    kill_stale_daemon(std::process::id());
}

// --- Tests for recoverable_session_pids and zombie exclusion ---

#[tokio::test]
async fn test_recoverable_session_pids_returns_resumable_pids() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        let mut session = test_session_info("amsterdam", Some(42));
        session.pid = Some(12345);
        state
            .headless_sessions
            .insert("amsterdam".to_string(), session);

        let mut session2 = test_session_info("columbus", Some(43));
        session2.pid = Some(67890);
        state
            .headless_sessions
            .insert("columbus".to_string(), session2);
    }

    let pids = recoverable_session_pids(&persistent_state).await;
    assert_eq!(pids.len(), 2);
    assert!(pids.contains(&12345));
    assert!(pids.contains(&67890));
}

#[tokio::test]
async fn test_recoverable_session_pids_excludes_non_resumable() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        let mut resumable = test_session_info("amsterdam", Some(42));
        resumable.pid = Some(11111);
        state
            .headless_sessions
            .insert("amsterdam".to_string(), resumable);

        let mut historical = test_session_info("columbus", Some(43));
        historical.pid = Some(22222);
        historical.resume_on_startup = false;
        state
            .headless_sessions
            .insert("columbus".to_string(), historical);
    }

    let pids = recoverable_session_pids(&persistent_state).await;
    assert_eq!(pids.len(), 1, "Only resumable sessions should be included");
    assert!(pids.contains(&11111));
    assert!(!pids.contains(&22222));
}

#[tokio::test]
async fn test_recoverable_session_pids_skips_sessions_without_pid() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        let mut no_pid_session = test_session_info("amsterdam", Some(42));
        no_pid_session.pid = None;
        state
            .headless_sessions
            .insert("amsterdam".to_string(), no_pid_session);
    }

    let pids = recoverable_session_pids(&persistent_state).await;
    assert!(
        pids.is_empty(),
        "Sessions without a PID should not appear in the exclusion set"
    );
}

#[tokio::test]
async fn test_recoverable_session_pids_empty_state() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    let pids = recoverable_session_pids(&persistent_state).await;
    assert!(pids.is_empty());
}

/// Verify that check_sandbox_context() returns an appropriate message when
/// the daemon is running inside a sandbox (where coworker sandboxing will fail).
///
/// This prevents the crash loop from 2026-02-13 where:
/// 1. Lead ran `midtown start --daemon-only` from within a sandboxed session
/// 2. Daemon inherited the Lead's sandbox
/// 3. All coworker spawns failed with "Already inside a sandbox — cannot nest sandbox-exec"
#[test]
#[cfg(target_os = "macos")]
fn test_check_sandbox_context_when_nested() {
    // Skip if we're already inside a sandbox (can't nest sandbox-exec)
    if !crate::sandbox::can_sandbox() {
        eprintln!("Skipping test: already inside a sandbox (nesting not allowed)");
        return;
    }

    // Run the check from inside a nested sandbox to simulate the failure scenario.
    // We spawn a child process under sandbox-exec and verify it detects the nesting.
    let profile_content = "(version 1)(allow default)";
    let tmp = std::env::temp_dir().join("midtown-test-startup-sandbox.sb");
    std::fs::write(&tmp, profile_content).expect("write test profile");

    let exe = std::env::current_exe().expect("current exe");
    let output = std::process::Command::new("sandbox-exec")
        .args(["-f", &tmp.to_string_lossy()])
        .arg(&exe)
        .args([
            "--test",
            "daemon::startup::tests::test_check_sandbox_context_detects_nesting",
        ])
        .arg("--exact")
        .arg("--nocapture")
        .output()
        .expect("spawn sandboxed test");

    let _ = std::fs::remove_file(&tmp);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() || stderr.contains("detected nested sandbox"),
        "Nested sandbox detection test failed: {}",
        stderr
    );
}

/// Helper test: verifies check_sandbox_context() returns Some(warning) when nested.
/// Called from test_check_sandbox_context_when_nested via sandbox-exec.
#[test]
#[cfg(target_os = "macos")]
fn test_check_sandbox_context_detects_nesting() {
    let warning = check_sandbox_context();

    if !crate::sandbox::can_sandbox() {
        // We're inside a sandbox — check_sandbox_context() should return a warning
        assert!(
            warning.is_some(),
            "check_sandbox_context() should return warning when nested"
        );
        let msg = warning.unwrap();
        assert!(
            msg.contains("already inside a sandbox"),
            "Warning should mention nested sandbox: {}",
            msg
        );
        assert!(
            msg.to_lowercase()
                .contains("coworker sandboxing will be disabled"),
            "Warning should mention disabled sandboxing: {}",
            msg
        );
        eprintln!("detected nested sandbox correctly: {}", msg);
    } else {
        // Not inside a sandbox — check_sandbox_context() should return None
        assert!(
            warning.is_none(),
            "check_sandbox_context() should return None when not nested"
        );
    }
}
