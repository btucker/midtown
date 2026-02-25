//! Tests for auth RPC handlers.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::auth::AuthProvider;

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
        execution_role_for_coworker(&lead, &reviewer_pr_by_name, &channel_lead_session_names),
        crate::config::ExecutionRole::Lead
    );

    let reviewer = crate::coworker::Coworker {
        name: "park".to_string(),
        ..lead.clone()
    };
    assert_eq!(
        execution_role_for_coworker(&reviewer, &reviewer_pr_by_name, &channel_lead_session_names),
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
            &channel_lead_session_names
        ),
        crate::config::ExecutionRole::ChannelLead
    );

    let coworker = crate::coworker::Coworker {
        name: "lexington".to_string(),
        ..lead
    };
    assert_eq!(
        execution_role_for_coworker(&coworker, &reviewer_pr_by_name, &channel_lead_session_names),
        crate::config::ExecutionRole::Coworker
    );
}
