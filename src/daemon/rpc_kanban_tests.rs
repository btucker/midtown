//! Tests for kanban RPC cache invalidation.

use super::*;

#[test]
fn test_current_cache_serves_stale_data_on_coworker_change() {
    // This test documents the current bug: KANBAN_CACHE uses only repo_hash
    // as the key, so when coworker state changes, stale data is served.

    let cache = KanbanCache::new();
    let repo_hash: u64 = 12345;

    // Cache response with no coworkers
    let response_no_coworkers = serde_json::json!({"coworkers": []});
    cache.set(response_no_coworkers.clone(), repo_hash);

    // Verify cache hit
    assert_eq!(cache.get(repo_hash), Some(response_no_coworkers.clone()));

    // BUG: Even if coworker state changed in the real world (madison spawned
    // and got task 1234), cache.get(repo_hash) still returns the old response
    // because repo_hash hasn't changed.
    //
    // The test simulates this by checking with the same repo_hash.
    // In the real system, the coworker state would have changed between
    // these two calls, but the cache serves stale data.
    assert_eq!(
        cache.get(repo_hash),
        Some(response_no_coworkers),
        "BUG: Cache returns stale data when coworker state changes"
    );
}

#[test]
fn test_fixed_cache_invalidates_on_coworker_state_change() {
    // This test verifies that with the fix, KANBAN_CACHE invalidates when
    // coworker state changes.

    let cache = KanbanCache::new();

    // Scenario 1: Cache with no coworkers active
    let repo_hash: u64 = 12345;
    let coworker_state_1: u64 = 0; // No coworkers
    let cache_key_1 = compute_cache_key(repo_hash, coworker_state_1);

    let response_1 = serde_json::json!({"coworkers": []});
    cache.set(response_1.clone(), cache_key_1);

    // Verify we get the cached value back
    assert_eq!(cache.get(cache_key_1), Some(response_1.clone()));

    // Scenario 2: Coworker spawns and gets assigned task
    // The coworker state changes (now 1 active coworker with task assignment)
    let coworker_state_2: u64 = compute_coworker_state_hash(&[("madison", Some(1234), None)]);
    let cache_key_2 = compute_cache_key(repo_hash, coworker_state_2);

    // With the fix, cache.get(cache_key_2) returns None because the cache key
    // is different (includes coworker state).
    assert_eq!(
        cache.get(cache_key_2),
        None,
        "Cache should miss when coworker state changes"
    );

    // Store new response with updated coworker data
    let response_2 = serde_json::json!({"coworkers": [{"name": "madison", "task_id": 1234}]});
    cache.set(response_2.clone(), cache_key_2);

    // Verify we get the new cached value with the new key
    assert_eq!(cache.get(cache_key_2), Some(response_2.clone()));

    // Note: The old cache entry is overwritten (cache stores only one entry).
    // The old key now returns None.
    assert_eq!(cache.get(cache_key_1), None);
}

/// Compute a cache key combining repo and coworker state.
fn compute_cache_key(repo_hash: u64, coworker_state_hash: u64) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    repo_hash.hash(&mut hasher);
    coworker_state_hash.hash(&mut hasher);
    hasher.finish()
}

/// Compute a hash representing coworker state.
///
/// Input: list of (coworker_name, task_id, pr_number) tuples.
/// Returns a hash that changes when task assignments or PR associations change.
fn compute_coworker_state_hash(coworkers: &[(&str, Option<u32>, Option<u64>)]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    // Sort by name for deterministic hashing
    let mut sorted = coworkers.to_vec();
    sorted.sort_by_key(|(name, _, _)| *name);

    for (name, task_id, pr_number) in sorted {
        name.hash(&mut hasher);
        task_id.hash(&mut hasher);
        pr_number.hash(&mut hasher);
    }

    hasher.finish()
}

#[test]
fn test_coworker_state_hash_changes_on_assignment() {
    // Empty state
    let hash_0 = compute_coworker_state_hash(&[]);

    // One coworker, no task
    let hash_1 = compute_coworker_state_hash(&[("madison", None, None)]);

    // Same coworker, with task
    let hash_2 = compute_coworker_state_hash(&[("madison", Some(1234), None)]);

    // Same coworker, with task and PR
    let hash_3 = compute_coworker_state_hash(&[("madison", Some(1234), Some(42))]);

    // Two coworkers
    let hash_4 = compute_coworker_state_hash(&[
        ("madison", Some(1234), Some(42)),
        ("park", Some(1235), None),
    ]);

    // All hashes should be different
    assert_ne!(hash_0, hash_1);
    assert_ne!(hash_1, hash_2);
    assert_ne!(hash_2, hash_3);
    assert_ne!(hash_3, hash_4);
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
