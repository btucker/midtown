//! Tests for kanban RPC cache invalidation.

use super::*;

// ============================================================================
// Tests for channel lead filtering — channel leads must not appear in the
// kanban coworker list (they are scoped to their specific channel)
// ============================================================================

#[test]
fn test_is_channel_lead_matches_registered_names() {
    let channel_leads: std::collections::HashSet<String> = ["auth", "web-interface", "daemon-core"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    assert!(is_channel_lead("auth", &channel_leads));
    assert!(is_channel_lead("web-interface", &channel_leads));
    assert!(is_channel_lead("daemon-core", &channel_leads));
}

#[test]
fn test_is_channel_lead_rejects_non_leads() {
    let channel_leads: std::collections::HashSet<String> = ["auth", "web-interface"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    assert!(!is_channel_lead("madison", &channel_leads));
    assert!(!is_channel_lead("broadway", &channel_leads));
    assert!(!is_channel_lead("amsterdam", &channel_leads));
    assert!(!is_channel_lead("park", &channel_leads));
    assert!(!is_channel_lead("lead", &channel_leads));
}

#[test]
fn test_is_channel_lead_empty_set() {
    let channel_leads: std::collections::HashSet<String> = std::collections::HashSet::new();

    // With no channel leads registered, nothing should match
    assert!(!is_channel_lead("auth", &channel_leads));
    assert!(!is_channel_lead("madison", &channel_leads));
}

// ============================================================================
// Tests for KanbanCache — repo-path-keyed cache for PR GraphQL data
//
// The kanban cache now keys only on repo paths. Coworker state is no longer
// part of the cache key — coworker data is served by `coworkers.status` at
// a faster refresh rate. This keeps the expensive GraphQL cache stable.
// ============================================================================

#[test]
fn test_kanban_cache_serves_pr_data_on_repo_hash() {
    let cache = KanbanCache::new();
    let repo_hash: u64 = 12345;

    // Cache PR response
    let response = serde_json::json!({"prs": [], "merged_prs": [], "repos": []});
    cache.set(response.clone(), repo_hash);

    // Verify cache hit with the same repo hash
    assert_eq!(cache.get(repo_hash), Some(response));
}

#[test]
fn test_kanban_cache_misses_on_different_repo_hash() {
    let cache = KanbanCache::new();
    let repo_hash_a: u64 = 12345;
    let repo_hash_b: u64 = 99999;

    let response = serde_json::json!({"prs": [], "merged_prs": []});
    cache.set(response.clone(), repo_hash_a);

    // Different repo hash → cache miss
    assert_eq!(cache.get(repo_hash_b), None);
}

// ============================================================================
// Tests for is_lead_health_active — bug regression: legacy "lead" key vs repo name
//
// is_lead_actively_working() used to hard-code the "lead" key, which always
// returned false for modern sessions named after the repo (e.g., "midtown").
// is_lead_health_active() takes an explicit health map + repo_name so it
// can be tested without a full DaemonState.
// ============================================================================

#[test]
fn test_is_lead_health_active_detects_by_repo_name() {
    // Modern sessions use the repo name (not "lead") as the health map key.
    // Bug: is_lead_actively_working() only looked up "lead", so this always returned false.
    let mut health = HashMap::new();
    health.insert(
        "midtown".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now()),
            ..Default::default()
        },
    );
    assert!(
        is_lead_health_active(&health, "midtown"),
        "Should detect activity via repo-name key (not just 'lead')"
    );
}

#[test]
fn test_is_lead_health_active_detects_by_legacy_lead_key() {
    // Legacy sessions use "lead" as the health map key — still must work.
    let mut health = HashMap::new();
    health.insert(
        "lead".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now()),
            ..Default::default()
        },
    );
    assert!(
        is_lead_health_active(&health, "midtown"),
        "Should detect activity via legacy 'lead' key"
    );
}

#[test]
fn test_is_lead_health_active_returns_false_when_absent() {
    // Neither "lead" nor repo-name key is present.
    let health: HashMap<String, ProcessHealth> = HashMap::new();
    assert!(!is_lead_health_active(&health, "midtown"));
}

#[test]
fn test_lead_activity_detection_no_health() {
    assert!(!is_session_actively_working(None));
}

#[test]
fn test_lead_activity_detection_not_alive() {
    let health = ProcessHealth {
        is_alive: false,
        last_event_at: Some(Utc::now()),
        ..Default::default()
    };
    assert!(!is_session_actively_working(Some(&health)));
}

#[test]
fn test_lead_activity_detection_alive_recent_event() {
    let health = ProcessHealth {
        is_alive: true,
        last_event_at: Some(Utc::now()),
        ..Default::default()
    };
    assert!(is_session_actively_working(Some(&health)));
}

#[test]
fn test_lead_activity_detection_alive_stale_event() {
    // Event older than LEAD_ACTIVITY_TIMEOUT — should be considered idle
    let stale_ts = Utc::now() - chrono::Duration::seconds(10);
    let health = ProcessHealth {
        is_alive: true,
        last_event_at: Some(stale_ts),
        ..Default::default()
    };
    assert!(!is_session_actively_working(Some(&health)));
}

#[test]
fn test_lead_activity_detection_alive_no_events() {
    // Alive but has never received any events
    let health = ProcessHealth {
        is_alive: true,
        last_event_at: None,
        ..Default::default()
    };
    assert!(!is_session_actively_working(Some(&health)));
}

#[test]
fn test_lead_activity_detection_future_timestamp() {
    // Clock skew: last_event_at is in the future — should NOT report active
    let future_ts = Utc::now() + chrono::Duration::seconds(60);
    let health = ProcessHealth {
        is_alive: true,
        last_event_at: Some(future_ts),
        ..Default::default()
    };
    assert!(!is_session_actively_working(Some(&health)));
}

#[test]
fn test_serialize_tool_activity_empty() {
    let map: HashMap<String, Vec<crate::universal_events::UniversalItem>> = HashMap::new();
    let result = serialize_tool_activity(&map);
    assert_eq!(result, serde_json::json!({}));
}

#[test]
fn test_serialize_tool_activity_with_tool_call() {
    use crate::universal_events::{ContentPart, ItemKind, ItemStatus, UniversalItem};

    let mut map = HashMap::new();
    map.insert(
        "amsterdam".to_string(),
        vec![UniversalItem {
            item_id: "call_001".to_string(),
            kind: ItemKind::ToolCall,
            content: vec![ContentPart::ToolCall {
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "git status"}),
                call_id: "call_001".to_string(),
                semantic_header: "$ git status".to_string(),
            }],
            status: ItemStatus::Completed,
            timestamp: Utc::now(),
        }],
    );

    let result = serialize_tool_activity(&map);
    let obj = result.as_object().expect("should be an object");
    assert!(obj.contains_key("amsterdam"));

    let items = obj["amsterdam"].as_array().expect("should be an array");
    assert_eq!(items.len(), 1);

    // Verify the JSON structure has the fields the TUI expects
    let item = &items[0];
    assert_eq!(item["item_id"], "call_001");
    let content = &item["content"][0]["ToolCall"];
    assert_eq!(content["semantic_header"], "$ git status");
    assert_eq!(content["name"], "Bash");
}

#[test]
fn test_serialize_tool_activity_with_tool_result() {
    use crate::universal_events::{ContentPart, ItemKind, ItemStatus, UniversalItem};

    let mut map = HashMap::new();
    map.insert(
        "madison".to_string(),
        vec![UniversalItem {
            item_id: "result:call_002".to_string(),
            kind: ItemKind::ToolResult,
            content: vec![ContentPart::ToolResult {
                call_id: "call_002".to_string(),
                output: "success".to_string(),
                is_error: false,
            }],
            status: ItemStatus::Completed,
            timestamp: Utc::now(),
        }],
    );

    let result = serialize_tool_activity(&map);
    let obj = result.as_object().expect("should be an object");
    let items = obj["madison"].as_array().expect("should be an array");
    assert_eq!(items.len(), 1);

    let item = &items[0];
    assert_eq!(item["item_id"], "result:call_002");
    let content = &item["content"][0]["ToolResult"];
    assert_eq!(content["call_id"], "call_002");
    assert!(!content["is_error"].as_bool().unwrap());
}

// ============================================================================
// Tests for project lead filtering — the lead must not appear in the
// coworker status list regardless of whether it uses the legacy "lead" name
// or the canonical repo name. Regression tests for !1723.
// ============================================================================

#[test]
fn test_is_project_lead_matches_legacy_name() {
    assert!(is_project_lead("lead", "midtown"));
    assert!(is_project_lead("Lead", "midtown"));
    assert!(is_project_lead("LEAD", "midtown"));
}

#[test]
fn test_is_project_lead_matches_repo_name() {
    // Canonical: lead session is named after the repo
    assert!(is_project_lead("midtown", "midtown"));
    assert!(is_project_lead("Midtown", "midtown"));
    assert!(is_project_lead("MIDTOWN", "MIDTOWN"));
}

#[test]
fn test_is_project_lead_rejects_regular_coworkers() {
    assert!(!is_project_lead("york", "midtown"));
    assert!(!is_project_lead("park", "midtown"));
    assert!(!is_project_lead("amsterdam", "midtown"));
    // Channel lead names are NOT project leads
    assert!(!is_project_lead("auth", "midtown"));
}
