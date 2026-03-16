//! Tests for session_id traceability in process respawn.
//!
//! These tests verify that session_id is populated from name_session_map
//! in `decide_dead_process_respawns`.

use std::collections::HashMap;

use chrono::Utc;

use super::decide_dead_process_respawns;
use crate::daemon::snapshot::ProcessHealth;

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
        has_tool_name_conflict: false,
        has_pending_api_call: false,
    }
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
