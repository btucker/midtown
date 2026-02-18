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

// ============================================================================
// Tests for compute_coworker_state_hash — cache key stability fix
// ============================================================================

/// Build a CoworkerRecord with the given phase, task_id, and progress.
fn make_record(
    phase: Option<crate::coworker_state::WorkflowPhase>,
    task_id: Option<u32>,
    progress: Option<u8>,
) -> crate::rules::CoworkerRecord {
    crate::rules::CoworkerRecord {
        workflow_phase: phase,
        task_id,
        progress,
        ..Default::default()
    }
}

#[test]
fn test_cache_key_stable_on_progress_update() {
    // Progress updates should NOT change the cache key — they were the root cause
    // of excessive GraphQL API calls (every `midtown state --progress N` caused
    // a cache miss and a new API call).

    use crate::coworker_state::WorkflowPhase;

    let mut records_before = HashMap::new();
    records_before.insert(
        "madison".to_string(),
        make_record(Some(WorkflowPhase::Developing), Some(1234), Some(20)),
    );

    let mut records_after = HashMap::new();
    records_after.insert(
        "madison".to_string(),
        make_record(Some(WorkflowPhase::Developing), Some(1234), Some(60)),
    );

    let empty_cl: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Use the real function from the parent module (not the local test helper below)
    let hash_before = super::compute_coworker_state_hash(&records_before, &empty_cl);
    let hash_after = super::compute_coworker_state_hash(&records_after, &empty_cl);

    assert_eq!(
        hash_before, hash_after,
        "Progress update (20→60) must NOT change the cache key"
    );
}

#[test]
fn test_cache_key_changes_on_phase_transition() {
    use crate::coworker_state::WorkflowPhase;

    let mut records_dev = HashMap::new();
    records_dev.insert(
        "york".to_string(),
        make_record(Some(WorkflowPhase::Developing), Some(1500), Some(80)),
    );

    let mut records_pr = HashMap::new();
    records_pr.insert(
        "york".to_string(),
        make_record(Some(WorkflowPhase::PullRequest), Some(1500), Some(90)),
    );

    let empty_cl: std::collections::HashSet<String> = std::collections::HashSet::new();
    let hash_dev = super::compute_coworker_state_hash(&records_dev, &empty_cl);
    let hash_pr = super::compute_coworker_state_hash(&records_pr, &empty_cl);

    assert_ne!(
        hash_dev, hash_pr,
        "Phase transition (Developing→PullRequest) must change the cache key"
    );
}

#[test]
fn test_cache_key_changes_on_task_assignment() {
    use crate::coworker_state::WorkflowPhase;

    let mut records_no_task = HashMap::new();
    records_no_task.insert(
        "lexington".to_string(),
        make_record(Some(WorkflowPhase::Claiming), None, None),
    );

    let mut records_with_task = HashMap::new();
    records_with_task.insert(
        "lexington".to_string(),
        make_record(Some(WorkflowPhase::Developing), Some(999), None),
    );

    let empty_cl: std::collections::HashSet<String> = std::collections::HashSet::new();
    let hash_no_task = super::compute_coworker_state_hash(&records_no_task, &empty_cl);
    let hash_with_task = super::compute_coworker_state_hash(&records_with_task, &empty_cl);

    assert_ne!(
        hash_no_task, hash_with_task,
        "Task assignment must change the cache key"
    );
}

#[test]
fn test_cache_key_changes_on_coworker_spawn() {
    use crate::coworker_state::WorkflowPhase;

    // No coworkers
    let records_empty: HashMap<String, crate::rules::CoworkerRecord> = HashMap::new();

    // One coworker spawned
    let mut records_one = HashMap::new();
    records_one.insert(
        "amsterdam".to_string(),
        make_record(Some(WorkflowPhase::Developing), Some(1200), None),
    );

    let empty_cl: std::collections::HashSet<String> = std::collections::HashSet::new();
    let hash_empty = super::compute_coworker_state_hash(&records_empty, &empty_cl);
    let hash_one = super::compute_coworker_state_hash(&records_one, &empty_cl);

    assert_ne!(
        hash_empty, hash_one,
        "Coworker spawn must change the cache key"
    );
}

#[test]
fn test_cache_key_stable_when_idle_coworker_updates_progress() {
    // Idle coworkers are excluded from the hash entirely, so their progress
    // updates definitely should not affect the cache key.
    use crate::coworker_state::WorkflowPhase;

    let mut records_idle_no_progress = HashMap::new();
    records_idle_no_progress.insert(
        "riverside".to_string(),
        make_record(Some(WorkflowPhase::Idle), Some(1400), None),
    );

    let mut records_idle_with_progress = HashMap::new();
    records_idle_with_progress.insert(
        "riverside".to_string(),
        make_record(Some(WorkflowPhase::Idle), Some(1400), Some(100)),
    );

    let empty_cl: std::collections::HashSet<String> = std::collections::HashSet::new();
    let hash_no_progress = super::compute_coworker_state_hash(&records_idle_no_progress, &empty_cl);
    let hash_with_progress =
        super::compute_coworker_state_hash(&records_idle_with_progress, &empty_cl);

    assert_eq!(
        hash_no_progress, hash_with_progress,
        "Idle coworker progress must not affect the cache key (idle coworkers are excluded)"
    );
}

/// Test that the full cache key (repo + coworker state + channel leads) changes
/// when the set of channel leads changes.
///
/// This covers a bug where `channel_lead_names` appeared in the kanban response
/// (`channel_leads` field) but was not included in the cache key — so when a
/// channel lead was added or removed, the cached response served stale data.
#[test]
fn test_cache_key_changes_when_channel_lead_set_changes() {
    use crate::coworker_state::WorkflowPhase;

    // Helper: compute the same combined key that handle_kanban_data computes.
    // repo_hash + coworker_state_hash + sorted channel_lead_names → final key.
    fn combined_key(
        repo_hash: u64,
        records: &HashMap<String, crate::rules::CoworkerRecord>,
        channel_leads: &std::collections::HashSet<String>,
    ) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let coworker_hash = super::compute_coworker_state_hash(records, channel_leads);

        let mut sorted_names: Vec<&String> = channel_leads.iter().collect();
        sorted_names.sort();

        let mut hasher = DefaultHasher::new();
        repo_hash.hash(&mut hasher);
        coworker_hash.hash(&mut hasher);
        sorted_names.hash(&mut hasher);
        hasher.finish()
    }

    let repo_hash: u64 = 42;

    let mut records = HashMap::new();
    records.insert(
        "madison".to_string(),
        make_record(Some(WorkflowPhase::Developing), Some(1500), None),
    );

    // No channel leads
    let no_leads: std::collections::HashSet<String> = std::collections::HashSet::new();
    // One channel lead added
    let with_lead: std::collections::HashSet<String> =
        ["auth"].iter().map(|s| s.to_string()).collect();
    // A different channel lead
    let with_other_lead: std::collections::HashSet<String> =
        ["tui"].iter().map(|s| s.to_string()).collect();

    let key_no_leads = combined_key(repo_hash, &records, &no_leads);
    let key_with_lead = combined_key(repo_hash, &records, &with_lead);
    let key_with_other_lead = combined_key(repo_hash, &records, &with_other_lead);

    // After the fix: adding a channel lead must change the cache key.
    assert_ne!(
        key_no_leads, key_with_lead,
        "Cache key must change when a channel lead is added"
    );
    // Adding a different channel lead must also produce a different key.
    assert_ne!(
        key_with_lead, key_with_other_lead,
        "Cache key must differ for different channel lead sets"
    );
}

#[test]
fn test_cache_key_stable_when_channel_lead_phase_changes() {
    // Channel leads are excluded from the kanban coworker list (they're scoped
    // to a specific topic channel). The hash function must mirror this exclusion:
    // a channel lead's phase or task change must NOT change the cache key.
    use crate::coworker_state::WorkflowPhase;

    // Channel leads use bare channel names (e.g. "auth", not "ch-auth")
    let channel_leads: std::collections::HashSet<String> =
        ["auth"].iter().map(|s| s.to_string()).collect();

    // Baseline: one regular coworker active
    let mut base_records = HashMap::new();
    base_records.insert(
        "madison".to_string(),
        make_record(Some(WorkflowPhase::Developing), Some(1500), None),
    );

    // Same regular coworker + a channel lead in Developing phase
    let mut records_with_cl_dev = base_records.clone();
    records_with_cl_dev.insert(
        "auth".to_string(),
        make_record(Some(WorkflowPhase::Developing), Some(777), None),
    );

    // Same regular coworker + the same channel lead, now in PullRequest phase
    let mut records_with_cl_pr = base_records.clone();
    records_with_cl_pr.insert(
        "auth".to_string(),
        make_record(Some(WorkflowPhase::PullRequest), Some(777), None),
    );

    let hash_base = super::compute_coworker_state_hash(&base_records, &channel_leads);
    let hash_with_cl_dev = super::compute_coworker_state_hash(&records_with_cl_dev, &channel_leads);
    let hash_with_cl_pr = super::compute_coworker_state_hash(&records_with_cl_pr, &channel_leads);

    // Channel leads must be invisible to the hash — adding one or changing its
    // phase/task must not change the cache key.
    assert_eq!(
        hash_base, hash_with_cl_dev,
        "Adding a channel lead must NOT change the cache key"
    );
    assert_eq!(
        hash_with_cl_dev, hash_with_cl_pr,
        "Channel lead phase change (Developing→PullRequest) must NOT change the cache key"
    );
}
