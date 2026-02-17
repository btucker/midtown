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

// ============================================================================
// Tests for recover_channel_lead_sessions_from()
// ============================================================================

/// Create a temporary channel directory structure with the given channel names.
///
/// Returns the temp dir (must be kept alive) and the base_dir path.
fn create_temp_channels(channel_names: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let base_dir = tmp.path().to_path_buf();
    let channels_dir = base_dir.join("channels");
    std::fs::create_dir_all(&channels_dir).expect("create channels dir");
    for name in channel_names {
        let channel_file = channels_dir.join(format!("{}.jsonl", name));
        std::fs::write(&channel_file, "").expect("create channel file");
    }
    (tmp, base_dir)
}

/// Create an archived channel file (has `.archived.jsonl` extension).
fn create_archived_channel(base_dir: &std::path::Path, channel_name: &str) {
    let channels_dir = base_dir.join("channels");
    std::fs::create_dir_all(&channels_dir).expect("create channels dir");
    let channel_file = channels_dir.join(format!("{}.archived.jsonl", channel_name));
    std::fs::write(&channel_file, "").expect("create archived channel file");
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
    // (midtown channel appears via channel.jsonl, not channels/midtown.jsonl)
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let base_dir = tmp.path().to_path_buf();
    // Create legacy channel.jsonl (detected as "midtown" channel by Channel::list)
    std::fs::write(base_dir.join("channel.jsonl"), "").expect("create channel.jsonl");

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
async fn test_recover_channel_lead_sessions_archived_channels_excluded() {
    // Only archived channels → no effects
    let (_tmp, base_dir) = create_temp_channels(&[]);
    create_archived_channel(&base_dir, "web-interface");
    create_archived_channel(&base_dir, "payments");

    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    assert!(
        effects.is_empty(),
        "Archived channels should produce no effects, got: {:?}",
        effects
    );
}

#[tokio::test]
async fn test_recover_channel_lead_sessions_fresh_spawn_no_saved_session() {
    // One active topic channel with no saved session → SpawnCoworker(Fresh) + SaveChannelLeadSession
    let (_tmp, base_dir) = create_temp_channels(&["web-interface"]);
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    let effects =
        recover_channel_lead_sessions_from(&persistent_state, "test-repo", &base_dir).await;

    // Expect: SpawnCoworker (Fresh) + SaveChannelLeadSession placeholder
    assert_eq!(effects.len(), 2, "Expected 2 effects, got: {:?}", effects);

    let spawn = effects
        .iter()
        .find(|e| matches!(e, Effect::SpawnCoworker(_)));
    let save = effects
        .iter()
        .find(|e| matches!(e, Effect::SaveChannelLeadSession { .. }));

    assert!(spawn.is_some(), "Should have a SpawnCoworker effect");
    assert!(
        save.is_some(),
        "Should have a SaveChannelLeadSession effect"
    );

    if let Some(Effect::SpawnCoworker(config)) = spawn {
        assert_eq!(config.name, "web-interface");
        assert_eq!(config.session_mode, crate::launch::SessionMode::Fresh);
        assert!(
            matches!(config.role, crate::launch::CoworkerRole::ChannelLead { .. }),
            "Role should be ChannelLead"
        );
    }

    if let Some(Effect::SaveChannelLeadSession {
        channel_name,
        session_id,
    }) = save
    {
        assert_eq!(channel_name, "web-interface");
        assert!(
            session_id.is_empty(),
            "Placeholder session_id should be empty"
        );
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
