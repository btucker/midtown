//! Tests for auth RPC handlers.

use super::*;
use crate::auth::AuthProvider;

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
