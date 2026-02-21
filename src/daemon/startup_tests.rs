use super::*;

use crate::daemon::effects::Effect;
use crate::daemon::state::DaemonPersistentState;
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
        channel: None,
        working_dir: Some("/tmp/test".to_string()),
        provider: None,
        profile: None,
        resume_on_startup: true,
        initial_prompt: None,
    }
}

#[tokio::test]
async fn test_recovering_coworker_names_returns_session_names() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        let record1 = test_session_record("sess-1", "amsterdam", "dev");
        state.sessions.insert("sess-1".to_string(), record1);
        let record2 = test_session_record("sess-2", "columbus", "dev");
        state.sessions.insert("sess-2".to_string(), record2);
    }

    let names = recovering_coworker_names(&persistent_state).await;
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"amsterdam".to_string()));
    assert!(names.contains(&"columbus".to_string()));
}

#[tokio::test]
async fn test_recovering_coworker_names_skips_non_resumable() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        let record1 = test_session_record("sess-1", "amsterdam", "dev");
        state.sessions.insert("sess-1".to_string(), record1);
        let mut record2 = test_session_record("sess-2", "columbus", "dev");
        record2.resume_on_startup = false;
        state.sessions.insert("sess-2".to_string(), record2);
    }

    let names = recovering_coworker_names(&persistent_state).await;
    assert_eq!(names, vec!["amsterdam".to_string()]);
}

#[tokio::test]
async fn test_recovering_coworker_names_skips_stopped() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        let record1 = test_session_record("sess-1", "amsterdam", "dev");
        state.sessions.insert("sess-1".to_string(), record1);
        let mut record2 = test_session_record("sess-2", "columbus", "dev");
        record2.is_running = false;
        state.sessions.insert("sess-2".to_string(), record2);
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
    let workdir = std::path::Path::new("/tmp/test-project");
    assert!(
        !verify_midtown_process(99999, workdir),
        "Non-existent PID should not verify as midtown"
    );
}

#[test]
fn test_verify_midtown_process_returns_false_for_non_midtown_process() {
    // PID 1 (launchd/init) is definitely not a midtown process
    let workdir = std::path::Path::new("/tmp/test-project");
    assert!(
        !verify_midtown_process(1, workdir),
        "PID 1 (init/launchd) should not verify as midtown"
    );
}

#[test]
fn test_kill_stale_daemon_skips_non_midtown_process() {
    // PID 1 (launchd/init) should be skipped because it's not a midtown process.
    // This test verifies that kill_stale_daemon doesn't attempt to kill
    // non-midtown processes (it just logs and returns).
    // If it incorrectly tried to kill PID 1, the test environment would error.
    let workdir = std::path::PathBuf::from("/tmp/test-project");
    kill_stale_daemon(1, &workdir);
    // If we get here without panic/error, the function correctly skipped PID 1
}

#[test]
fn test_kill_stale_daemon_skips_own_pid() {
    // Should be a no-op when called with our own PID
    let workdir = std::path::PathBuf::from("/tmp/test-project");
    kill_stale_daemon(std::process::id(), &workdir);
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

// ── Channel lead session recovery tests ───────────────────────────────

/// Create temporary channel directories for testing.
///
/// Creates the per-channel directory layout: `channels/<name>/history/current.jsonl`.
/// Returns the temp dir (must be kept alive) and the base_dir path.
fn create_temp_channels(channel_names: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let base_dir = tmp.path().to_path_buf();
    let channels_dir = base_dir.join("channels");
    std::fs::create_dir_all(&channels_dir).expect("create channels dir");
    for name in channel_names {
        let history_dir = channels_dir.join(name).join("history");
        std::fs::create_dir_all(&history_dir).expect("create history dir");
        std::fs::write(history_dir.join("current.jsonl"), "").expect("create channel file");
    }
    (tmp, base_dir)
}

/// Create an archived channel directory (has `.archived` directory suffix).
fn create_archived_channel(base_dir: &std::path::Path, channel_name: &str) {
    let archived_dir = base_dir
        .join("channels")
        .join(format!("{}.archived", channel_name));
    let history_dir = archived_dir.join("history");
    std::fs::create_dir_all(&history_dir).expect("create archived history dir");
    std::fs::write(history_dir.join("current.jsonl"), "").expect("create archived channel file");
}

#[tokio::test]
async fn test_recover_channel_lead_sessions_empty() {
    // No channels → no effects
    let (_tmp, base_dir) = create_temp_channels(&[]);
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    assert!(
        effects.is_empty(),
        "No channels should produce no effects, got: {:?}",
        effects
    );
}

#[tokio::test]
async fn test_recover_channel_lead_sessions_only_midtown_excluded() {
    // Only the main "midtown" channel — no topic channels → no effects
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let base_dir = tmp.path().to_path_buf();
    // Create midtown channel using the new directory layout
    let history_dir = base_dir.join("channels").join("midtown").join("history");
    std::fs::create_dir_all(&history_dir).expect("create history dir");
    std::fs::write(history_dir.join("current.jsonl"), "").expect("create channel.jsonl");

    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    assert!(
        effects.is_empty(),
        "Only midtown channel should produce no effects, got: {:?}",
        effects
    );
}

#[tokio::test]
async fn test_recover_channel_lead_sessions_archived_excluded() {
    // Only an archived channel → no effects
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let base_dir = tmp.path().to_path_buf();
    create_archived_channel(&base_dir, "old-feature");

    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    assert!(
        effects.is_empty(),
        "Only archived channel should produce no effects, got: {:?}",
        effects
    );
}

#[tokio::test]
async fn test_recover_channel_lead_sessions_fresh_spawn() {
    // One active topic channel without a saved session → SpawnCoworker(Fresh) + placeholder
    let (_tmp, base_dir) = create_temp_channels(&["auth"]);
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    // Expect: SpawnCoworker(Fresh) + SaveChannelLeadSession placeholder
    assert_eq!(effects.len(), 2, "Expected 2 effects, got: {:?}", effects);

    match &effects[0] {
        Effect::SpawnCoworker(config) => {
            assert_eq!(config.name, "auth");
            assert_eq!(config.session_mode, crate::launch::SessionMode::Fresh);
        }
        other => panic!("Expected SpawnCoworker, got {:?}", other),
    }

    match &effects[1] {
        Effect::SaveChannelLeadSession {
            channel_name,
            session_id,
        } => {
            assert_eq!(channel_name, "auth");
            assert!(
                session_id.is_empty(),
                "Placeholder should have empty session_id"
            );
        }
        other => panic!("Expected SaveChannelLeadSession, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recover_channel_lead_sessions_resume_with_saved_session() {
    // One active topic channel with a saved session ID → SpawnCoworker(ResumeSession)
    let (_tmp, base_dir) = create_temp_channels(&["payments"]);

    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut ps = persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert("payments".to_string(), "session-abc-123".to_string());
    }

    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    // Expect: only SpawnCoworker(ResumeSession) — no SaveChannelLeadSession since entry exists
    assert_eq!(effects.len(), 1, "Expected 1 effect, got: {:?}", effects);

    match &effects[0] {
        Effect::SpawnCoworker(config) => {
            assert_eq!(config.name, "payments");
            assert_eq!(
                config.session_mode,
                crate::launch::SessionMode::ResumeSession("session-abc-123".to_string())
            );
        }
        other => panic!("Expected SpawnCoworker, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recover_channel_lead_sessions_empty_session_id_spawns_fresh() {
    // Saved session entry exists but with empty string → should spawn fresh
    let (_tmp, base_dir) = create_temp_channels(&["auth"]);

    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut ps = persistent_state.lock().await;
        // Empty session ID (placeholder set during a previous fresh spawn)
        ps.channel_lead_sessions
            .insert("auth".to_string(), String::new());
    }

    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    // With an empty session ID, should spawn fresh (not resume)
    // No SaveChannelLeadSession since entry already exists in the map
    assert_eq!(effects.len(), 1, "Expected 1 effect, got: {:?}", effects);

    match &effects[0] {
        Effect::SpawnCoworker(config) => {
            assert_eq!(config.name, "auth");
            assert_eq!(config.session_mode, crate::launch::SessionMode::Fresh);
        }
        other => panic!("Expected SpawnCoworker(Fresh), got {:?}", other),
    }
}

#[tokio::test]
async fn test_recover_channel_lead_sessions_multiple_channels() {
    // Two topic channels: one with a saved session, one without
    let (_tmp, base_dir) = create_temp_channels(&["web-interface", "payments"]);

    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut ps = persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert("payments".to_string(), "session-pay-456".to_string());
        // web-interface has no saved session
    }

    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    // payments: 1 SpawnCoworker(Resume)
    // web-interface: 1 SpawnCoworker(Fresh) + 1 SaveChannelLeadSession placeholder
    assert_eq!(effects.len(), 3, "Expected 3 effects, got: {:?}", effects);

    let spawns: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnCoworker(_)))
        .collect();
    let saves: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::SaveChannelLeadSession { .. }))
        .collect();

    assert_eq!(spawns.len(), 2, "Should have 2 SpawnCoworker effects");
    assert_eq!(
        saves.len(),
        1,
        "Should have 1 SaveChannelLeadSession effect"
    );

    // Verify the resume is for payments
    let resume = spawns.iter().find(|e| {
        matches!(
            e,
            Effect::SpawnCoworker(c) if c.session_mode == crate::launch::SessionMode::ResumeSession("session-pay-456".to_string())
        )
    });
    assert!(resume.is_some(), "Should have a resume for 'payments'");

    // Verify the fresh is for web-interface
    let fresh = spawns.iter().find(|e| {
        matches!(
            e,
            Effect::SpawnCoworker(c)
                if c.name == "web-interface" && c.session_mode == crate::launch::SessionMode::Fresh
        )
    });
    assert!(
        fresh.is_some(),
        "Should have a fresh spawn for 'web-interface'"
    );

    // Verify the placeholder save is for web-interface
    if let Some(Effect::SaveChannelLeadSession {
        channel_name,
        session_id,
    }) = saves.first().copied()
    {
        assert_eq!(channel_name, "web-interface");
        assert!(session_id.is_empty());
    }
}

/// Regression test for the stale session ID crash on daemon restart.
///
/// Scenario: A channel lead session previously failed to resume (e.g., 'No conversation found').
/// The death handler cleared `headless_sessions[name].session_id` but did NOT clear
/// `channel_lead_sessions[channel_name]`. On next daemon restart, the stale ID was used
/// to attempt another resume — crashing the session again in a loop.
///
/// The fix: `recover_channel_lead_sessions_from()` cross-checks `headless_sessions`
/// before attempting resume. If `headless_sessions[name].session_id` is empty (already
/// cleared), spawn fresh even if `channel_lead_sessions` still has a stale ID.
#[tokio::test]
async fn test_recover_channel_lead_sessions_skips_resume_when_headless_session_id_cleared() {
    // channel_lead_sessions has a stale session ID (not yet cleared by the death handler fix)
    // but headless_sessions[name].session_id is empty (was cleared after failed resume)
    let (_tmp, base_dir) = create_temp_channels(&["auth"]);

    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut ps = persistent_state.lock().await;
        // Stale session ID in channel_lead_sessions — the old bug: not cleared on failed resume
        ps.channel_lead_sessions
            .insert("auth".to_string(), "stale-session-id-xyz".to_string());
        // headless_sessions[name].session_id is empty — cleared by death handler
        let mut session = test_session_info("auth", None);
        session.session_id = String::new(); // cleared after failed resume
        ps.headless_sessions.insert("auth".to_string(), session);
    }

    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    // Should spawn Fresh (not ResumeSession with stale ID)
    assert_eq!(effects.len(), 1, "Expected 1 effect, got: {:?}", effects);

    match &effects[0] {
        Effect::SpawnCoworker(config) => {
            assert_eq!(config.name, "auth");
            assert_eq!(
                config.session_mode,
                crate::launch::SessionMode::Fresh,
                "Should spawn Fresh when headless_sessions session_id is empty, but got Resume with stale ID"
            );
        }
        other => panic!("Expected SpawnCoworker(Fresh), got {:?}", other),
    }
}

/// Companion test: when headless_sessions still has a valid (non-empty) session ID,
/// the stale-session cross-check should NOT interfere — resume should proceed normally.
#[tokio::test]
async fn test_recover_channel_lead_sessions_resumes_when_headless_session_id_matches() {
    let (_tmp, base_dir) = create_temp_channels(&["auth"]);

    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut ps = persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert("auth".to_string(), "valid-session-abc".to_string());
        // headless_sessions[name].session_id is non-empty (session was healthy)
        let session = test_session_info("auth", None); // session_id = "session-auth"
        ps.headless_sessions.insert("auth".to_string(), session);
    }

    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    // Should still resume — don't regress the happy path
    assert_eq!(effects.len(), 1, "Expected 1 effect, got: {:?}", effects);

    match &effects[0] {
        Effect::SpawnCoworker(config) => {
            assert_eq!(config.name, "auth");
            assert_eq!(
                config.session_mode,
                crate::launch::SessionMode::ResumeSession("valid-session-abc".to_string()),
                "Should resume when headless session is healthy"
            );
        }
        other => panic!("Expected SpawnCoworker(ResumeSession), got {:?}", other),
    }
}

#[tokio::test]
async fn test_recover_channel_lead_sessions_mixed_archived_and_active() {
    // Mix of active and archived channels — only active topic channels get leads
    let (_tmp, base_dir) = create_temp_channels(&["auth", "billing"]);
    create_archived_channel(&base_dir, "old-feature");

    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    // auth + billing → 2 SpawnCoworker(Fresh) + 2 SaveChannelLeadSession placeholders
    // old-feature (archived) → excluded
    assert_eq!(effects.len(), 4, "Expected 4 effects, got: {:?}", effects);

    let spawns: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnCoworker(_)))
        .collect();
    assert_eq!(spawns.len(), 2);

    let channel_names: Vec<_> = spawns
        .iter()
        .filter_map(|e| {
            if let Effect::SpawnCoworker(c) = e {
                Some(c.name.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(channel_names.contains(&"auth"), "auth should be spawned");
    assert!(
        channel_names.contains(&"billing"),
        "billing should be spawned"
    );
    assert!(
        !channel_names.contains(&"old-feature"),
        "old-feature should not be spawned"
    );
}

// ── Session record recovery tests ─────────────────────────────────────

/// Helper to create a test SessionRecord with sensible defaults.
fn test_session_record(
    session_id: &str,
    name: &str,
    coworker_type: &str,
) -> crate::daemon::state::SessionRecord {
    crate::daemon::state::SessionRecord {
        session_id: session_id.to_string(),
        task_id: None,
        current_name: Some(name.to_string()),
        preferred_name: Some(name.to_string()),
        working_dir: "/tmp/worktree".to_string(),
        branch: None,
        pr_number: None,
        initial_prompt: None,
        is_reviewer: false,
        coworker_type: coworker_type.to_string(),
        is_running: true,
        created_at: Utc::now(),
        resume_on_startup: true,
    }
}

#[tokio::test]
async fn test_recover_from_session_records_generates_resume_effects() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut state = persistent_state.lock().await;
        let mut record = test_session_record("sess-abc", "park", "dev");
        record.task_id = Some("42".to_string());
        state.sessions.insert("sess-abc".to_string(), record);
    }

    let (effects, recovered_ids) =
        recover_from_session_records(&persistent_state, "test-repo").await;

    assert_eq!(effects.len(), 1);
    assert!(recovered_ids.contains("sess-abc"));
    match &effects[0] {
        Effect::ResumeCoworker {
            name, session_id, ..
        } => {
            assert_eq!(name, "park");
            assert_eq!(session_id, "sess-abc");
        }
        other => panic!("Expected ResumeCoworker, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recover_from_session_records_skips_non_resumable() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut state = persistent_state.lock().await;
        let mut record = test_session_record("sess-abc", "park", "dev");
        record.resume_on_startup = false;
        state.sessions.insert("sess-abc".to_string(), record);
    }

    let (effects, recovered_ids) =
        recover_from_session_records(&persistent_state, "test-repo").await;

    assert!(
        effects.is_empty(),
        "Non-resumable sessions should not produce effects"
    );
    assert!(
        recovered_ids.is_empty(),
        "Non-resumable sessions should not be in recovered set"
    );
}

#[tokio::test]
async fn test_recover_from_session_records_skips_channel_leads() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut state = persistent_state.lock().await;
        let record = test_session_record("sess-cl", "auth", "channel-lead");
        state.sessions.insert("sess-cl".to_string(), record);
    }

    let (effects, recovered_ids) =
        recover_from_session_records(&persistent_state, "test-repo").await;

    assert!(
        effects.is_empty(),
        "Channel lead sessions should be skipped (recovered separately)"
    );
    assert!(
        recovered_ids.is_empty(),
        "Channel lead sessions should not be in recovered set"
    );
}

#[tokio::test]
async fn test_recover_from_session_records_reviewer_with_pr() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut state = persistent_state.lock().await;
        let mut record = test_session_record("sess-rev", "amsterdam", "reviewer");
        record.is_reviewer = true;
        record.pr_number = Some(123);
        state.sessions.insert("sess-rev".to_string(), record);
    }

    let (effects, recovered_ids) =
        recover_from_session_records(&persistent_state, "test-repo").await;

    assert_eq!(effects.len(), 1);
    assert!(recovered_ids.contains("sess-rev"));
    match &effects[0] {
        Effect::ResumeCoworker {
            name,
            session_id,
            config,
        } => {
            assert_eq!(name, "amsterdam");
            assert_eq!(session_id, "sess-rev");
            assert_eq!(config.pr_number, Some(123));
        }
        other => panic!("Expected ResumeCoworker, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recover_from_session_records_reviewer_without_pr_skipped() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut state = persistent_state.lock().await;
        let mut record = test_session_record("sess-rev", "amsterdam", "reviewer");
        record.is_reviewer = true;
        // No pr_number set
        state.sessions.insert("sess-rev".to_string(), record);
    }

    let (effects, recovered_ids) =
        recover_from_session_records(&persistent_state, "test-repo").await;

    assert!(
        effects.is_empty(),
        "Reviewer without PR number should be skipped"
    );
    assert!(
        recovered_ids.is_empty(),
        "Skipped reviewer should not be in recovered set"
    );
}

#[tokio::test]
async fn test_recover_from_session_records_restores_working_dir() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut state = persistent_state.lock().await;
        let mut record = test_session_record("sess-abc", "park", "dev");
        record.working_dir = "/home/user/.midtown/worktrees/repo/park".to_string();
        state.sessions.insert("sess-abc".to_string(), record);
    }

    let (effects, _) = recover_from_session_records(&persistent_state, "test-repo").await;

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::ResumeCoworker { config, .. } => {
            assert_eq!(
                config.working_dir,
                Some(std::path::PathBuf::from(
                    "/home/user/.midtown/worktrees/repo/park"
                ))
            );
        }
        other => panic!("Expected ResumeCoworker, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recover_from_session_records_empty() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    let (effects, recovered_ids) =
        recover_from_session_records(&persistent_state, "test-repo").await;

    assert!(effects.is_empty());
    assert!(recovered_ids.is_empty());
}

#[tokio::test]
async fn test_recover_from_session_records_uses_preferred_name() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut state = persistent_state.lock().await;
        let mut record = test_session_record("sess-abc", "old-name", "dev");
        record.current_name = Some("old-name".to_string());
        record.preferred_name = Some("preferred-name".to_string());
        state.sessions.insert("sess-abc".to_string(), record);
    }

    let (effects, _) = recover_from_session_records(&persistent_state, "test-repo").await;

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::ResumeCoworker { name, .. } => {
            assert_eq!(name, "preferred-name");
        }
        other => panic!("Expected ResumeCoworker, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recover_from_session_records_falls_back_to_current_name() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut state = persistent_state.lock().await;
        let mut record = test_session_record("sess-abc", "current", "dev");
        record.preferred_name = None;
        record.current_name = Some("current".to_string());
        state.sessions.insert("sess-abc".to_string(), record);
    }

    let (effects, _) = recover_from_session_records(&persistent_state, "test-repo").await;

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::ResumeCoworker { name, .. } => {
            assert_eq!(name, "current");
        }
        other => panic!("Expected ResumeCoworker, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recovering_coworker_names_multiple_session_records() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        let record1 = test_session_record("sess-abc", "park", "dev");
        state.sessions.insert("sess-abc".to_string(), record1);
        let record2 = test_session_record("sess-def", "amsterdam", "dev");
        state.sessions.insert("sess-def".to_string(), record2);
    }

    let names = recovering_coworker_names(&persistent_state).await;
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"amsterdam".to_string()));
    assert!(names.contains(&"park".to_string()));
}

/// Regression test: recover_from_session_records must use LaunchConfig::lead()
/// for the lead session, not LaunchConfig::coworker(). Without this fix, the
/// lead was recovered with model=sonnet and role=Coworker instead of
/// model=opus and role=Lead.
#[tokio::test]
async fn test_recover_from_session_records_uses_lead_config_for_lead() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    {
        let mut state = persistent_state.lock().await;
        // The lead's SessionRecord has coworker_type="dev", not "lead"
        let record = test_session_record("sess-lead", "lead", "dev");
        state.sessions.insert("sess-lead".to_string(), record);
    }

    let (effects, recovered_ids) =
        recover_from_session_records(&persistent_state, "test-repo").await;

    assert_eq!(effects.len(), 1);
    assert!(recovered_ids.contains("sess-lead"));

    match &effects[0] {
        Effect::ResumeCoworker {
            name,
            session_id,
            config,
        } => {
            assert_eq!(name, "lead");
            assert_eq!(session_id, "sess-lead");
            assert_eq!(
                config.role,
                crate::launch::CoworkerRole::Lead,
                "Lead should use CoworkerRole::Lead, not Coworker"
            );
            assert_eq!(
                config.model, "opus",
                "Lead should use opus model, not sonnet"
            );
        }
        other => panic!("Expected ResumeCoworker, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recovering_coworker_names_deduplicates() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        // Two session records with the same preferred name
        let record1 = test_session_record("sess-old", "park", "dev");
        state.sessions.insert("sess-old".to_string(), record1);
        let record2 = test_session_record("sess-new", "park", "dev");
        state.sessions.insert("sess-new".to_string(), record2);
    }

    let names = recovering_coworker_names(&persistent_state).await;
    assert_eq!(
        names.len(),
        1,
        "Duplicate names should be deduplicated, got: {:?}",
        names
    );
    assert!(names.contains(&"park".to_string()));
}

#[tokio::test]
async fn test_recover_from_session_records_skips_stopped_sessions() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        // Running session — should be recovered
        let mut running = test_session_record("sess-running", "park", "dev");
        running.is_running = true;
        running.resume_on_startup = true;
        state.sessions.insert("sess-running".to_string(), running);

        // Stopped session — should NOT be recovered
        let mut stopped = test_session_record("sess-stopped", "lexington", "dev");
        stopped.is_running = false;
        stopped.resume_on_startup = true;
        state.sessions.insert("sess-stopped".to_string(), stopped);
    }

    let (effects, _) = recover_from_session_records(&persistent_state, "test-repo").await;

    // Only the running session should produce an effect
    assert_eq!(effects.len(), 1, "Should only recover running sessions");
    match &effects[0] {
        Effect::ResumeCoworker { config, .. } => {
            assert_eq!(config.name, "park");
        }
        other => panic!("Expected ResumeCoworker, got {:?}", other),
    }
}

#[tokio::test]
async fn test_recover_from_session_records_deduplicates_by_name() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        // Older session for "park"
        let mut older = test_session_record("sess-old", "park", "dev");
        older.is_running = true;
        older.resume_on_startup = true;
        older.created_at = chrono::Utc::now() - chrono::Duration::hours(2);
        state.sessions.insert("sess-old".to_string(), older);

        // Newer session for "park" — should win
        let mut newer = test_session_record("sess-new", "park", "dev");
        newer.is_running = true;
        newer.resume_on_startup = true;
        newer.created_at = chrono::Utc::now();
        state.sessions.insert("sess-new".to_string(), newer);
    }

    let (effects, _) = recover_from_session_records(&persistent_state, "test-repo").await;

    // Should only produce one effect despite two sessions with the same name
    assert_eq!(effects.len(), 1, "Should deduplicate by name");
    match &effects[0] {
        Effect::ResumeCoworker {
            config, session_id, ..
        } => {
            assert_eq!(config.name, "park");
            assert_eq!(session_id, "sess-new", "Should use the newer session");
        }
        other => panic!("Expected ResumeCoworker, got {:?}", other),
    }
}

// ── clear_stale_running_sessions tests ────────────────────────────────

/// On daemon restart, sessions with is_running=True but resume_on_startup=False
/// are skipped by recover_from_session_records — but their is_running flag was
/// never cleared, causing dispatch to treat them as active indefinitely.
///
/// This test verifies that clear_stale_running_sessions() resets is_running to
/// false for any session not included in the recovered set (excluding channel leads).
#[tokio::test]
async fn test_clear_stale_running_sessions_clears_non_resumed() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        // This session will be recovered (resume_on_startup=true, is_running=true)
        let active = test_session_record("sess-active", "park", "dev");
        state.sessions.insert("sess-active".to_string(), active);

        // This session was running before restart but won't be resumed
        // (resume_on_startup=false). Its is_running flag is stale.
        let mut stale = test_session_record("sess-stale", "lexington", "dev");
        stale.resume_on_startup = false;
        state.sessions.insert("sess-stale".to_string(), stale);
    }

    // Simulate recovery: only "sess-active" was recovered
    let mut recovered = std::collections::HashSet::new();
    recovered.insert("sess-active".to_string());

    let active_channels: std::collections::HashSet<String> = std::collections::HashSet::new();
    clear_stale_running_sessions(&persistent_state, &recovered, &active_channels).await;

    let state = persistent_state.lock().await;
    assert!(
        state.sessions["sess-active"].is_running,
        "Recovered session should remain is_running=true"
    );
    assert!(
        !state.sessions["sess-stale"].is_running,
        "Stale session (not recovered) should have is_running cleared to false"
    );
}

/// Active channel lead sessions are recovered separately via recover_channel_lead_sessions.
/// clear_stale_running_sessions must NOT clear their is_running flag when the channel
/// is still active (non-archived).
#[tokio::test]
async fn test_clear_stale_running_sessions_preserves_channel_leads() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        // Channel lead — recovered separately, must not be touched
        let lead = test_session_record("sess-lead", "payments", "channel-lead");
        state.sessions.insert("sess-lead".to_string(), lead);
        // "payments" is an active channel
        state
            .channel_lead_sessions
            .insert("payments".to_string(), "sess-lead".to_string());
    }

    // Recovered set is empty (channel leads go through a different path)
    let recovered = std::collections::HashSet::new();

    // "payments" is an active (non-archived) channel
    let mut active_channels: std::collections::HashSet<String> = std::collections::HashSet::new();
    active_channels.insert("payments".to_string());

    clear_stale_running_sessions(&persistent_state, &recovered, &active_channels).await;

    let state = persistent_state.lock().await;
    assert!(
        state.sessions["sess-lead"].is_running,
        "Channel lead sessions for active channels must not be cleared by clear_stale_running_sessions"
    );
}

/// Reviewer sessions have resume_on_startup=false and are never resumed.
/// Their stale is_running=true should be cleared.
#[tokio::test]
async fn test_clear_stale_running_sessions_clears_stale_reviewers() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        let mut reviewer = test_session_record("sess-reviewer", "amsterdam", "reviewer");
        reviewer.is_reviewer = true;
        reviewer.pr_number = Some(42);
        reviewer.resume_on_startup = false;
        state.sessions.insert("sess-reviewer".to_string(), reviewer);
    }

    let recovered = std::collections::HashSet::new();
    let active_channels: std::collections::HashSet<String> = std::collections::HashSet::new();
    clear_stale_running_sessions(&persistent_state, &recovered, &active_channels).await;

    let state = persistent_state.lock().await;
    assert!(
        !state.sessions["sess-reviewer"].is_running,
        "Stale reviewer session should have is_running cleared"
    );
}

/// When there are no sessions at all, clear_stale_running_sessions is a no-op.
#[tokio::test]
async fn test_clear_stale_running_sessions_empty_state() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    let recovered = std::collections::HashSet::new();
    let active_channels: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Must not panic
    clear_stale_running_sessions(&persistent_state, &recovered, &active_channels).await;
    let state = persistent_state.lock().await;
    assert!(state.sessions.is_empty());
}

/// Archived channel-lead sessions have is_running=true but their channel no longer exists
/// as an active channel. clear_stale_running_sessions must clear their flag so dispatch
/// doesn't treat them as still active.
///
/// Regression test for: a topic channel archived between daemon runs causes its
/// channel-lead SessionRecord to retain is_running=true permanently (neither
/// clear_stale_running_sessions nor recover_channel_lead_sessions touches it).
#[tokio::test]
async fn test_clear_stale_running_sessions_clears_archived_channel_lead() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        // Channel lead session for "old-feature", which has since been archived
        let lead = test_session_record("sess-archived-lead", "old-feature", "channel-lead");
        state
            .sessions
            .insert("sess-archived-lead".to_string(), lead);

        // channel_lead_sessions does NOT contain "old-feature" because the archive
        // effect removes the entry. However, the SessionRecord still has is_running=true.
        // (No entry in channel_lead_sessions for "old-feature")
    }

    // Active channels: empty (the channel was archived and its entry removed)
    let active_channels: std::collections::HashSet<String> = std::collections::HashSet::new();
    let recovered = std::collections::HashSet::new();

    clear_stale_running_sessions(&persistent_state, &recovered, &active_channels).await;

    let state = persistent_state.lock().await;
    assert!(
        !state.sessions["sess-archived-lead"].is_running,
        "Channel-lead session for archived channel must have is_running cleared to false"
    );
}

/// Active (non-archived) channel-lead sessions must NOT be cleared by
/// clear_stale_running_sessions — they are recovered separately.
#[tokio::test]
async fn test_clear_stale_running_sessions_preserves_active_channel_lead() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        // Active channel lead — channel is NOT archived
        let lead = test_session_record("sess-active-lead", "payments", "channel-lead");
        state.sessions.insert("sess-active-lead".to_string(), lead);

        // payments is an active channel lead session
        state
            .channel_lead_sessions
            .insert("payments".to_string(), "sess-active-lead".to_string());
    }

    // "payments" is an active channel
    let mut active_channels: std::collections::HashSet<String> = std::collections::HashSet::new();
    active_channels.insert("payments".to_string());

    let recovered = std::collections::HashSet::new();

    clear_stale_running_sessions(&persistent_state, &recovered, &active_channels).await;

    let state = persistent_state.lock().await;
    assert!(
        state.sessions["sess-active-lead"].is_running,
        "Channel-lead session for active channel must NOT be cleared"
    );
}

/// Sessions already marked is_running=false are not affected.
#[tokio::test]
async fn test_clear_stale_running_sessions_skips_already_stopped() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        let mut stopped = test_session_record("sess-stopped", "york", "dev");
        stopped.is_running = false;
        stopped.resume_on_startup = false;
        state.sessions.insert("sess-stopped".to_string(), stopped);
    }

    let recovered = std::collections::HashSet::new();
    let active_channels: std::collections::HashSet<String> = std::collections::HashSet::new();
    clear_stale_running_sessions(&persistent_state, &recovered, &active_channels).await;

    let state = persistent_state.lock().await;
    assert!(
        !state.sessions["sess-stopped"].is_running,
        "Already-stopped session should remain stopped"
    );
}
