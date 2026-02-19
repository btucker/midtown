//! Tests for session_id traceability in stuck detection and process respawn.
//!
//! These tests verify that session_id is populated from name_session_map
//! in `decide_stuck_coworker_restarts`, `decide_dead_process_respawns`,
//! and `decide_stuck_reviewer_restarts`.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::Utc;

use super::{
    StuckExemptions, decide_dead_process_respawns, decide_stuck_coworker_restarts,
    decide_stuck_reviewer_restarts,
};
use crate::daemon::snapshot::ProcessHealth;

fn stuck_health(now: chrono::DateTime<Utc>) -> ProcessHealth {
    ProcessHealth {
        is_alive: true,
        last_event_at: Some(now - chrono::Duration::minutes(10)),
        has_usage_limit: false,
        usage_limit_reset_at: None,
        has_api_error: false,
        has_auth_error: false,
        has_running_subagent: false,
        has_pending_tool: false,
        has_pending_api_call: false,
        has_tool_name_conflict: false,
        exit_code: None,
    }
}

fn dead_health(exit_code: i32) -> ProcessHealth {
    ProcessHealth {
        is_alive: false,
        exit_code: Some(exit_code),
        last_event_at: Some(Utc::now() - chrono::Duration::seconds(60)),
        has_usage_limit: false,
        usage_limit_reset_at: None,
        has_api_error: false,
        has_auth_error: false,
        has_running_subagent: false,
        has_pending_tool: false,
        has_pending_api_call: false,
        has_tool_name_conflict: false,
    }
}

#[test]
fn stuck_coworker_restart_includes_session_id_from_map() {
    let now = Utc::now();
    let mut health_map = HashMap::new();
    health_map.insert("riverside".to_string(), stuck_health(now));
    let tasks = vec![(
        "42".to_string(),
        "Fix bug".to_string(),
        "riverside".to_string(),
    )];
    let exemptions = StuckExemptions {
        usage_limited: &HashSet::new(),
        api_error: &HashSet::new(),
        auth_error: &HashSet::new(),
        attached: &HashMap::new(),
    };
    let mut name_session_map = HashMap::new();
    name_session_map.insert("riverside".to_string(), "session-abc-123".to_string());

    let restarts = decide_stuck_coworker_restarts(
        &health_map,
        &tasks,
        &exemptions,
        now,
        Duration::from_secs(180),
        &name_session_map,
    );

    assert_eq!(restarts.len(), 1);
    assert_eq!(
        restarts[0].session_id,
        Some("session-abc-123".to_string()),
        "session_id should be populated from name_session_map"
    );
}

#[test]
fn stuck_coworker_restart_session_id_none_when_no_mapping() {
    let now = Utc::now();
    let mut health_map = HashMap::new();
    health_map.insert("riverside".to_string(), stuck_health(now));
    let tasks = vec![(
        "42".to_string(),
        "Fix bug".to_string(),
        "riverside".to_string(),
    )];
    let exemptions = StuckExemptions {
        usage_limited: &HashSet::new(),
        api_error: &HashSet::new(),
        auth_error: &HashSet::new(),
        attached: &HashMap::new(),
    };

    let restarts = decide_stuck_coworker_restarts(
        &health_map,
        &tasks,
        &exemptions,
        now,
        Duration::from_secs(180),
        &HashMap::new(),
    );

    assert_eq!(restarts.len(), 1);
    assert_eq!(
        restarts[0].session_id, None,
        "session_id should be None when no name_session_map entry exists"
    );
}

#[test]
fn dead_process_respawn_includes_session_id_from_map() {
    let mut health = HashMap::new();
    health.insert("york".to_string(), dead_health(1));

    let tasks = vec![("42".to_string(), "Fix bug".to_string(), "york".to_string())];

    let mut name_session_map = HashMap::new();
    name_session_map.insert("york".to_string(), "session-dead-456".to_string());

    let respawns = decide_dead_process_respawns(&health, &tasks, &name_session_map);

    assert_eq!(respawns.len(), 1);
    assert_eq!(
        respawns[0].session_id,
        Some("session-dead-456".to_string()),
        "session_id should be populated from name_session_map"
    );
}

#[test]
fn dead_process_respawn_session_id_none_when_no_mapping() {
    let mut health = HashMap::new();
    health.insert("york".to_string(), dead_health(1));

    let tasks = vec![("42".to_string(), "Fix bug".to_string(), "york".to_string())];

    let respawns = decide_dead_process_respawns(&health, &tasks, &HashMap::new());

    assert_eq!(respawns.len(), 1);
    assert_eq!(
        respawns[0].session_id, None,
        "session_id should be None when no name_session_map entry exists"
    );
}

#[test]
fn stuck_reviewer_restart_includes_session_id_from_map() {
    let now = Utc::now();
    let mut health_map = HashMap::new();
    health_map.insert("riverside".to_string(), stuck_health(now));
    let mut assignments = HashMap::new();
    assignments.insert("riverside".to_string(), 42u64);
    let exemptions = StuckExemptions {
        usage_limited: &HashSet::new(),
        api_error: &HashSet::new(),
        auth_error: &HashSet::new(),
        attached: &HashMap::new(),
    };
    let mut name_session_map = HashMap::new();
    name_session_map.insert("riverside".to_string(), "session-rev-789".to_string());

    let restarts = decide_stuck_reviewer_restarts(
        &health_map,
        &assignments,
        &HashMap::new(),
        &exemptions,
        now,
        Duration::from_secs(300),
        2,
        &name_session_map,
    );

    assert_eq!(restarts.len(), 1);
    assert_eq!(
        restarts[0].session_id,
        Some("session-rev-789".to_string()),
        "session_id should be populated from name_session_map"
    );
}

#[test]
fn stuck_reviewer_restart_session_id_none_when_no_mapping() {
    let now = Utc::now();
    let mut health_map = HashMap::new();
    health_map.insert("riverside".to_string(), stuck_health(now));
    let mut assignments = HashMap::new();
    assignments.insert("riverside".to_string(), 42u64);
    let exemptions = StuckExemptions {
        usage_limited: &HashSet::new(),
        api_error: &HashSet::new(),
        auth_error: &HashSet::new(),
        attached: &HashMap::new(),
    };

    let restarts = decide_stuck_reviewer_restarts(
        &health_map,
        &assignments,
        &HashMap::new(),
        &exemptions,
        now,
        Duration::from_secs(300),
        2,
        &HashMap::new(),
    );

    assert_eq!(restarts.len(), 1);
    assert_eq!(
        restarts[0].session_id, None,
        "session_id should be None when no name_session_map entry exists"
    );
}
