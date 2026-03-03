//! Tests for auth RPC handlers.

use std::collections::{HashMap, HashSet};
use std::process::Command;

use super::*;
use crate::auth::AuthProvider;

// ============================================================================
// Integration test helper
// ============================================================================

/// Build a minimal DaemonState wired to a temp directory.
///
/// Returns:
/// - `DaemonState` — the state to pass to handlers under test
/// - `tempfile::TempDir` — the git repo root; keep alive for the test
/// - `tempfile::TempDir` — the midtown base dir; keep alive for the test
/// - `crate::paths::TestMidtownBaseDirGuard` — resets the override on drop
fn make_pool_toggle_test_state(
    repo_name: &str,
) -> (
    DaemonState,
    tempfile::TempDir,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    // Point all auth/config filesystem reads to a fresh temp directory.
    let midtown_dir = tempfile::tempdir().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    // Minimal git repo so DaemonState::new succeeds.
    let repo_dir = tempfile::tempdir().expect("repo temp dir");
    Command::new("git")
        .args(["init"])
        .current_dir(repo_dir.path())
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(repo_dir.path())
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(repo_dir.path())
        .output()
        .expect("git config name");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(repo_dir.path())
        .output()
        .expect("git commit");

    let wm =
        crate::worktree::WorktreeManager::new(repo_dir.path().to_path_buf()).expect("worktree mgr");
    let cm = crate::coworker::CoworkerManager::new(wm);
    let channel_router = crate::ChannelRouter::new(repo_dir.path(), "midtown");
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);

    let state = DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        repo_name.to_string(),
        vec![repo_dir.path().to_path_buf()],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state");

    (state, repo_dir, midtown_dir, _guard)
}

/// Create a real profile directory so `profile_exists_for` returns `true`.
///
/// For Claude, the profile directory is `<midtown_base>/auth/<name>/claude/`.
fn create_profile_dir(midtown_dir: &tempfile::TempDir, provider: AuthProvider, name: &str) {
    let base = midtown_dir.path().join("auth");
    let dir = match provider {
        AuthProvider::Claude => base.join(name).join("claude"),
        AuthProvider::Codex => base
            .join("providers")
            .join("codex")
            .join("profiles")
            .join(name),
        AuthProvider::Zai => base
            .join("providers")
            .join("zai")
            .join("profiles")
            .join(name),
    };
    std::fs::create_dir_all(&dir).expect("create profile dir");
}

// ============================================================================
// handle_auth_pool_toggle integration tests
// ============================================================================

// ============================================================================
// Pool toggle config logic tests
// ============================================================================

/// Apply a pool toggle (add/remove) to a `FullProjectConfig` in memory.
///
/// Mirrors the logic in `handle_auth_pool_toggle` without requiring a
/// `DaemonState` — lets us unit-test the core mutation in isolation.
fn apply_pool_toggle(config: &mut crate::config::FullProjectConfig, profile: &str, enabled: bool) {
    if enabled {
        let profiles = config
            .execution
            .coworker_profiles
            .get_or_insert_with(Vec::new);
        if !profiles.contains(&profile.to_string()) {
            profiles.push(profile.to_string());
        }
    } else if let Some(profiles) = config.execution.coworker_profiles.as_mut() {
        profiles.retain(|p| p != profile);
    }
}

#[test]
fn test_pool_toggle_add_to_none_initializes_list() {
    let mut config = crate::config::FullProjectConfig::default();
    assert!(config.execution.coworker_profiles.is_none());

    apply_pool_toggle(&mut config, "alice@example.com", true);

    assert_eq!(
        config.execution.coworker_profiles,
        Some(vec!["alice@example.com".to_string()])
    );
}

#[test]
fn test_pool_toggle_add_is_idempotent() {
    let mut config = crate::config::FullProjectConfig::default();
    config.execution.coworker_profiles = Some(vec!["alice@example.com".to_string()]);

    apply_pool_toggle(&mut config, "alice@example.com", true);
    apply_pool_toggle(&mut config, "alice@example.com", true);

    assert_eq!(
        config.execution.coworker_profiles,
        Some(vec!["alice@example.com".to_string()])
    );
}

#[test]
fn test_pool_toggle_add_multiple_profiles() {
    let mut config = crate::config::FullProjectConfig::default();

    apply_pool_toggle(&mut config, "alice@example.com", true);
    apply_pool_toggle(&mut config, "bob@example.com", true);

    assert_eq!(
        config.execution.coworker_profiles,
        Some(vec![
            "alice@example.com".to_string(),
            "bob@example.com".to_string(),
        ])
    );
}

#[test]
fn test_pool_toggle_remove_profile() {
    let mut config = crate::config::FullProjectConfig::default();
    config.execution.coworker_profiles = Some(vec![
        "alice@example.com".to_string(),
        "bob@example.com".to_string(),
    ]);

    apply_pool_toggle(&mut config, "alice@example.com", false);

    assert_eq!(
        config.execution.coworker_profiles,
        Some(vec!["bob@example.com".to_string()])
    );
}

#[test]
fn test_pool_toggle_disable_on_unset_leaves_none() {
    // Regression for P1: disabling when coworker_profiles is None must not
    // create Some([]), which would shadow inherited global pool entries.
    let mut config = crate::config::FullProjectConfig::default();
    assert!(config.execution.coworker_profiles.is_none());

    apply_pool_toggle(&mut config, "alice@example.com", false);

    assert!(
        config.execution.coworker_profiles.is_none(),
        "disabling a profile when the list is unset must not initialize it to Some([])"
    );
}

#[test]
fn test_pool_toggle_remove_is_idempotent() {
    let mut config = crate::config::FullProjectConfig::default();
    config.execution.coworker_profiles = Some(vec!["alice@example.com".to_string()]);

    apply_pool_toggle(&mut config, "bob@example.com", false);
    apply_pool_toggle(&mut config, "bob@example.com", false);

    // alice is still there; bob was never in the list
    assert_eq!(
        config.execution.coworker_profiles,
        Some(vec!["alice@example.com".to_string()])
    );
}

#[test]
fn test_pool_toggle_remove_last_leaves_empty_vec() {
    let mut config = crate::config::FullProjectConfig::default();
    config.execution.coworker_profiles = Some(vec!["alice@example.com".to_string()]);

    apply_pool_toggle(&mut config, "alice@example.com", false);

    assert_eq!(config.execution.coworker_profiles, Some(vec![]));
}

#[test]
fn test_pool_toggle_config_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");

    // Write initial config with one profile
    let mut config = crate::config::FullProjectConfig::default();
    config.execution.coworker_profiles = Some(vec!["alice@example.com".to_string()]);
    config.save_to(&path).expect("save_to");

    // Load, toggle in bob, save again
    let mut loaded = crate::config::FullProjectConfig::load_from(&path).unwrap();
    apply_pool_toggle(&mut loaded, "bob@example.com", true);
    loaded.save_to(&path).expect("save_to after toggle");

    // Reload and verify both profiles are present
    let reloaded = crate::config::FullProjectConfig::load_from(&path).unwrap();
    let profiles = reloaded.execution.coworker_profiles.unwrap_or_default();
    assert!(profiles.contains(&"alice@example.com".to_string()));
    assert!(profiles.contains(&"bob@example.com".to_string()));
}

// ============================================================================
// handle_auth_pool_toggle integration tests
// ============================================================================
//
// These tests call the production function with a real DaemonState and verify
// the full round-trip: request validation → config mutation → disk persistence
// → ops-channel broadcast → JSON response shape.

/// Enable a profile that exists → success response + persisted config.
#[tokio::test]
async fn test_pool_toggle_enable_adds_profile_full_round_trip() {
    let (state, _repo, midtown_dir, _guard) = make_pool_toggle_test_state("test-repo");
    create_profile_dir(&midtown_dir, AuthProvider::Claude, "alice@example.com");

    let resp = handle_auth_pool_toggle(
        crate::rpc::RequestId::Number(1),
        AuthProvider::Claude,
        "alice@example.com",
        true,
        &state,
    )
    .await;

    // Response must be success with the expected shape.
    let result = resp.result.expect("expected success, got error");
    assert_eq!(result["success"], true);
    assert_eq!(result["profile"], "alice@example.com");
    assert_eq!(result["provider"], "claude");
    assert_eq!(result["enabled"], true);
    let profiles = result["coworker_profiles"]
        .as_array()
        .expect("coworker_profiles must be an array");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0], "alice@example.com");

    // Config must be persisted to disk.
    let config_path = crate::config::project_config_path("test-repo");
    let saved = crate::config::FullProjectConfig::load_from(&config_path)
        .expect("config must have been saved");
    assert_eq!(
        saved.execution.coworker_profiles,
        Some(vec!["alice@example.com".to_string()])
    );
}

/// Disable a profile that's in the list → removed from config on disk.
#[tokio::test]
async fn test_pool_toggle_disable_removes_profile_full_round_trip() {
    let (state, _repo, _midtown_dir, _guard) = make_pool_toggle_test_state("test-repo");

    // Pre-populate config with alice in the pool.
    let config_path = crate::config::project_config_path("test-repo");
    std::fs::create_dir_all(config_path.parent().unwrap()).expect("create config dir");
    let mut config = crate::config::FullProjectConfig::default();
    config.execution.coworker_profiles = Some(vec!["alice@example.com".to_string()]);
    config.save_to(&config_path).expect("pre-populate config");

    // Disable does not check profile existence — no need to create a profile dir.
    let resp = handle_auth_pool_toggle(
        crate::rpc::RequestId::Number(2),
        AuthProvider::Claude,
        "alice@example.com",
        false,
        &state,
    )
    .await;

    let result = resp.result.expect("expected success, got error");
    assert_eq!(result["success"], true);
    assert_eq!(result["enabled"], false);
    let profiles = result["coworker_profiles"]
        .as_array()
        .expect("coworker_profiles array");
    assert!(
        profiles.is_empty(),
        "alice should have been removed from the pool"
    );

    // Disk must reflect the removal.
    let saved =
        crate::config::FullProjectConfig::load_from(&config_path).expect("saved config exists");
    assert_eq!(
        saved.execution.coworker_profiles,
        Some(vec![]),
        "alice must be gone from the persisted pool"
    );
}

/// P1 regression: disabling when coworker_profiles is None must not write
/// `Some([])` to disk, which would shadow inherited global pool entries.
#[tokio::test]
async fn test_pool_toggle_disable_on_unset_does_not_persist_empty_list() {
    let (state, _repo, _midtown_dir, _guard) = make_pool_toggle_test_state("test-repo");

    let resp = handle_auth_pool_toggle(
        crate::rpc::RequestId::Number(3),
        AuthProvider::Claude,
        "nobody@example.com",
        false,
        &state,
    )
    .await;

    // Handler should succeed (removing a non-existent entry is idempotent).
    resp.result.expect("expected success, got error");

    // No config file should have been written yet (or, if it was, the field is absent / None).
    let config_path = crate::config::project_config_path("test-repo");
    if config_path.exists() {
        let saved = crate::config::FullProjectConfig::load_from(&config_path).unwrap();
        assert!(
            saved.execution.coworker_profiles.is_none(),
            "coworker_profiles must remain None — not Some([]) — after disabling a non-existent entry"
        );
    }
}

/// Enabling a profile that does not exist on disk → -32602 error.
#[tokio::test]
async fn test_pool_toggle_enable_nonexistent_profile_returns_error() {
    let (state, _repo, _midtown_dir, _guard) = make_pool_toggle_test_state("test-repo");
    // Intentionally do NOT create the profile directory.

    let resp = handle_auth_pool_toggle(
        crate::rpc::RequestId::Number(4),
        AuthProvider::Claude,
        "ghost@example.com",
        true,
        &state,
    )
    .await;

    let err = resp.error.expect("expected error for nonexistent profile");
    assert_eq!(err.code, -32602, "invalid params error code");
    assert!(
        err.message.contains("ghost@example.com"),
        "error message should name the missing profile"
    );
}

/// Invalid profile name (path traversal) → -32602 error, no disk write.
#[tokio::test]
async fn test_pool_toggle_invalid_profile_name_returns_error() {
    let (state, _repo, _midtown_dir, _guard) = make_pool_toggle_test_state("test-repo");

    let resp = handle_auth_pool_toggle(
        crate::rpc::RequestId::Number(5),
        AuthProvider::Claude,
        "../etc/passwd",
        true,
        &state,
    )
    .await;

    let err = resp.error.expect("expected error for invalid profile name");
    assert_eq!(err.code, -32602);
    assert!(
        err.message.contains("Invalid profile name"),
        "error should mention invalid name, got: {}",
        err.message
    );
}

/// Enabling the same profile twice → profile appears only once in the list.
#[tokio::test]
async fn test_pool_toggle_enable_is_idempotent_via_handler() {
    let (state, _repo, midtown_dir, _guard) = make_pool_toggle_test_state("test-repo");
    create_profile_dir(&midtown_dir, AuthProvider::Claude, "alice@example.com");

    // Enable alice twice.
    for req_id in [10i64, 11] {
        handle_auth_pool_toggle(
            crate::rpc::RequestId::Number(req_id),
            AuthProvider::Claude,
            "alice@example.com",
            true,
            &state,
        )
        .await
        .result
        .expect("enable should succeed");
    }

    let config_path = crate::config::project_config_path("test-repo");
    let saved =
        crate::config::FullProjectConfig::load_from(&config_path).expect("config was saved");
    let profiles = saved.execution.coworker_profiles.unwrap_or_default();
    assert_eq!(
        profiles
            .iter()
            .filter(|p| *p == "alice@example.com")
            .count(),
        1,
        "alice must appear exactly once after two enable calls"
    );
}

// ============================================================================
// execution_role_for_coworker — canonical lead name
// ============================================================================

/// A session named after the repo (e.g., "midtown") must be identified as Lead.
///
/// Regression: before this fix, `execution_role_for_coworker` only checked for
/// the legacy literal "lead", so a modern canonical session got `Coworker` role
/// and was restarted with coworker credentials after an auth-profile switch.
#[test]
fn test_execution_role_for_canonical_lead_name() {
    let reviewer_pr: HashMap<String, u64> = HashMap::new();
    let channel_leads: HashSet<String> = HashSet::new();

    let canonical_lead = crate::coworker::Coworker {
        slot_id: "1".to_string(),
        name: "midtown".to_string(), // canonical name — NOT the legacy "lead"
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/midtown".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "opus".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: "default".to_string(),
    };

    assert_eq!(
        execution_role_for_coworker(&canonical_lead, &reviewer_pr, &channel_leads, "midtown"),
        crate::config::ExecutionRole::Lead,
        "canonical repo-named session must be classified as Lead, not Coworker"
    );
}

/// Legacy "lead" name must still be identified as Lead after the refactor.
#[test]
fn test_execution_role_for_legacy_lead_name_still_works() {
    let reviewer_pr: HashMap<String, u64> = HashMap::new();
    let channel_leads: HashSet<String> = HashSet::new();

    let legacy_lead = crate::coworker::Coworker {
        slot_id: "1".to_string(),
        name: "lead".to_string(),
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/lead".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "opus".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: "default".to_string(),
    };

    assert_eq!(
        execution_role_for_coworker(&legacy_lead, &reviewer_pr, &channel_leads, "midtown"),
        crate::config::ExecutionRole::Lead,
        "legacy 'lead' session must still be classified as Lead"
    );
}

/// After a successful toggle, the handler broadcasts to the ops channel so web
/// UI clients receive the update without polling.
#[tokio::test]
async fn test_pool_toggle_broadcasts_to_ops_channel() {
    let (state, repo_dir, midtown_dir, _guard) = make_pool_toggle_test_state("test-repo");
    create_profile_dir(&midtown_dir, AuthProvider::Claude, "alice@example.com");

    handle_auth_pool_toggle(
        crate::rpc::RequestId::Number(20),
        AuthProvider::Claude,
        "alice@example.com",
        true,
        &state,
    )
    .await
    .result
    .expect("toggle should succeed");

    // The ChannelRouter was created with repo_dir as base_dir; read the ops channel.
    let router = crate::ChannelRouter::new(repo_dir.path(), "midtown");
    let ops = router.get_channel("ops").expect("ops channel exists");
    let messages = ops.read_all().expect("read ops channel");

    let found = messages
        .iter()
        .any(|m| m.content.contains("alice@example.com") && m.content.contains("coworker pool"));
    assert!(
        found,
        "ops channel must contain a broadcast about alice@example.com joining the pool; messages: {:?}",
        messages.iter().map(|m| &m.content).collect::<Vec<_>>()
    );
}

/// Codex provider: enabling a profile creates the correct provider-scoped
/// directory path and the response returns `"provider": "codex"`.
#[tokio::test]
async fn test_pool_toggle_codex_provider_enable() {
    let (state, _repo, midtown_dir, _guard) = make_pool_toggle_test_state("test-repo");
    create_profile_dir(&midtown_dir, AuthProvider::Codex, "codex-user@example.com");

    let resp = handle_auth_pool_toggle(
        crate::rpc::RequestId::Number(30),
        AuthProvider::Codex,
        "codex-user@example.com",
        true,
        &state,
    )
    .await;

    let result = resp.result.expect("codex enable should succeed");
    assert_eq!(result["provider"], "codex");
    assert_eq!(result["enabled"], true);
    let profiles = result["coworker_profiles"]
        .as_array()
        .expect("coworker_profiles array");
    assert!(profiles.iter().any(|p| p == "codex-user@example.com"));
}

#[test]
fn test_filter_coworkers_by_provider() {
    let coworkers = vec![
        crate::coworker::Coworker {
            slot_id: "1".to_string(),
            name: "lexington".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp/lexington".to_string(),
            started_at: chrono::Utc::now(),
            current_task: Some("Build auth".to_string()),
            session_id: None,
            model: "sonnet".to_string(),
            provider: crate::auth::AuthProvider::Claude,
            profile: "default".to_string(),
        },
        crate::coworker::Coworker {
            slot_id: "2".to_string(),
            name: "park".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp/park".to_string(),
            started_at: chrono::Utc::now(),
            current_task: Some("Review PR".to_string()),
            session_id: None,
            model: "gpt-5-codex".to_string(),
            provider: crate::auth::AuthProvider::Codex,
            profile: "default".to_string(),
        },
    ];

    let claude = filter_coworkers_by_provider(&coworkers, crate::auth::AuthProvider::Claude);
    let codex = filter_coworkers_by_provider(&coworkers, crate::auth::AuthProvider::Codex);

    assert_eq!(claude.len(), 1);
    assert_eq!(claude[0].name, "lexington");
    assert_eq!(codex.len(), 1);
    assert_eq!(codex[0].name, "park");
}

#[test]
fn test_build_coworker_relaunch_config_preserves_name_and_model() {
    let coworker = crate::coworker::Coworker {
        slot_id: "1".to_string(),
        name: "madison".to_string(),
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/madison".to_string(),
        started_at: chrono::Utc::now(),
        current_task: Some("Fix tests".to_string()),
        session_id: None,
        model: "opus".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: "default".to_string(),
    };

    let config = build_coworker_relaunch_config(&coworker, "midtown");
    assert_eq!(config.name, "madison");
    assert_eq!(config.model, "opus");
    assert_eq!(config.session_mode, crate::launch::SessionMode::Resume);
}

#[test]
fn test_lead_relaunch_status_strings() {
    assert_eq!(LeadRelaunchStatus::Relaunched.as_str(), "relaunched");
    assert_eq!(LeadRelaunchStatus::Failed.as_str(), "failed");
    assert_eq!(LeadRelaunchStatus::Unchanged.as_str(), "unchanged");
    assert_eq!(LeadRelaunchStatus::Unchanged.summary(), "lead unchanged");
    assert!(!LeadRelaunchStatus::Unchanged.attempted());
    assert!(LeadRelaunchStatus::Relaunched.relaunched());
}

#[test]
fn test_build_coworker_relaunch_config_preserves_old_provider() {
    // This test documents that build_coworker_relaunch_config() copies
    // the provider from the coworker record. This is INTENTIONAL for
    // most callers, but handle_auth_switch() must override it.
    let coworker = crate::coworker::Coworker {
        slot_id: "1".to_string(),
        name: "lexington".to_string(),
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/lexington".to_string(),
        started_at: chrono::Utc::now(),
        current_task: Some("Build auth".to_string()),
        session_id: None,
        model: "sonnet".to_string(),
        provider: AuthProvider::Claude,
        profile: "old-profile".to_string(),
    };

    let config = build_coworker_relaunch_config(&coworker, "midtown");

    // The config preserves the coworker's provider
    assert_eq!(config.auth_provider, AuthProvider::Claude);
    // handle_auth_switch() must override this with the NEW provider
}

#[test]
fn test_provider_platform_compatibility() {
    assert_eq!(
        platform_for_provider(AuthProvider::Claude),
        SessionPlatform::ClaudeCli
    );
    assert_eq!(
        platform_for_provider(AuthProvider::Zai),
        SessionPlatform::ClaudeCli
    );
    assert_eq!(
        platform_for_provider(AuthProvider::Codex),
        SessionPlatform::CodexCli
    );
    assert!(can_resume_between_providers(
        AuthProvider::Claude,
        AuthProvider::Zai
    ));
    assert!(can_resume_between_providers(
        AuthProvider::Zai,
        AuthProvider::Claude
    ));
    assert!(!can_resume_between_providers(
        AuthProvider::Claude,
        AuthProvider::Codex
    ));
    assert!(!can_resume_between_providers(
        AuthProvider::Codex,
        AuthProvider::Zai
    ));
}

#[test]
fn test_execution_role_for_coworker() {
    let mut reviewer_pr_by_name = HashMap::new();
    reviewer_pr_by_name.insert("park".to_string(), 42);

    let channel_lead_session_names = HashSet::from(["auth".to_string()]);

    let lead = crate::coworker::Coworker {
        slot_id: "1".to_string(),
        name: "lead".to_string(),
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/lead".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "opus".to_string(),
        provider: AuthProvider::Claude,
        profile: "default".to_string(),
    };
    assert_eq!(
        execution_role_for_coworker(
            &lead,
            &reviewer_pr_by_name,
            &channel_lead_session_names,
            "myrepo"
        ),
        crate::config::ExecutionRole::Lead
    );

    let reviewer = crate::coworker::Coworker {
        name: "park".to_string(),
        ..lead.clone()
    };
    assert_eq!(
        execution_role_for_coworker(
            &reviewer,
            &reviewer_pr_by_name,
            &channel_lead_session_names,
            "myrepo"
        ),
        crate::config::ExecutionRole::Reviewer
    );

    let channel_lead = crate::coworker::Coworker {
        name: "auth".to_string(),
        ..lead.clone()
    };
    assert_eq!(
        execution_role_for_coworker(
            &channel_lead,
            &reviewer_pr_by_name,
            &channel_lead_session_names,
            "myrepo"
        ),
        crate::config::ExecutionRole::ChannelLead
    );

    let coworker = crate::coworker::Coworker {
        name: "lexington".to_string(),
        ..lead
    };
    assert_eq!(
        execution_role_for_coworker(
            &coworker,
            &reviewer_pr_by_name,
            &channel_lead_session_names,
            "myrepo"
        ),
        crate::config::ExecutionRole::Coworker
    );
}

// ============================================================================
// handle_auth_switch — global fan-out force flag
// ============================================================================

/// Regression: when the CLI fan-out sends `auth.switch(all=true, force=true)` to
/// multiple daemons sequentially, the first daemon writes the global profile.
/// Subsequent daemons see `current == profile && cleared == 0` — without force,
/// they early-return WITHOUT restarting sessions, leaving stale credentials.
///
/// With `force=true`, the handler must bypass the early-return and restart.
#[tokio::test]
async fn test_auth_switch_global_force_bypasses_early_return() {
    let (state, _repo, midtown_dir, _guard) = make_pool_toggle_test_state("test-repo");

    let profile_name = "alice@example.com";
    create_profile_dir(&midtown_dir, AuthProvider::Claude, profile_name);

    // Pre-set the global profile to alice (simulating Daemon A already wrote it).
    crate::auth::set_current_profile_for(AuthProvider::Claude, profile_name)
        .expect("pre-set profile");

    assert_eq!(
        crate::auth::current_profile_for(AuthProvider::Claude),
        profile_name,
        "precondition: profile should already be set"
    );

    // Call without force — should early-return with switched=false.
    let resp_no_force = handle_auth_switch(
        crate::rpc::RequestId::Number(100),
        profile_name,
        true,  // all=true (global switch)
        false, // force=false
        AuthProvider::Claude,
        &state,
    )
    .await;
    let result_no_force = resp_no_force.result.expect("expected success");
    assert_eq!(
        result_no_force["switched"], false,
        "without force, handler should early-return when profile already matches"
    );

    // Call with force — should restart sessions (switched=true).
    let resp_force = handle_auth_switch(
        crate::rpc::RequestId::Number(101),
        profile_name,
        true, // all=true (global switch)
        true, // force=true
        AuthProvider::Claude,
        &state,
    )
    .await;
    let result_force = resp_force.result.expect("expected success");
    assert_eq!(
        result_force["switched"], true,
        "with force=true, handler must restart sessions even when profile already matches"
    );
}
