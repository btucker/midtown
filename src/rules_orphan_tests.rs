//! Tests for orphan recovery with spawn-failure cooldown filtering.
//!
//! When multiple coworkers die simultaneously, orphan recovery must skip
//! cooldown-blocked owners and try the next orphan, rather than returning
//! None and deadlocking all recovery.

use std::collections::{HashMap, HashSet};

use super::{OrphanRecoveryContext, decide_orphan_recovery};

fn empty_set() -> HashSet<String> {
    HashSet::new()
}

fn names(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn skips_cooldown_blocked_owner_recovers_next_orphan() {
    // Two orphaned tasks: "alpha" is on cooldown, "bravo" is not.
    // Recovery should skip alpha and return bravo.
    let tasks = vec![
        ("1".to_string(), "task one".to_string(), "alpha".to_string()),
        ("2".to_string(), "task two".to_string(), "bravo".to_string()),
    ];
    let empty = empty_set();
    let empty_map: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let cooldown = names(&["alpha"]);
    let ctx = OrphanRecoveryContext {
        in_progress: &tasks,
        active_names: &empty,
        recently_stopped: &empty,
        attached_coworkers: &empty_map,
        channel_lead_names: &empty,
        spawn_failure_cooldown_names: &cooldown,
    };
    let result = decide_orphan_recovery(&ctx);
    assert!(result.is_some(), "should recover a non-cooldown orphan");
    let recovery = result.unwrap();
    assert_eq!(recovery.task_id, "2");
    assert_eq!(recovery.owner, "bravo");
}

#[test]
fn all_owners_on_cooldown_returns_none() {
    // All orphaned task owners are on cooldown — recovery should return None.
    let tasks = vec![
        ("1".to_string(), "task one".to_string(), "alpha".to_string()),
        ("2".to_string(), "task two".to_string(), "bravo".to_string()),
    ];
    let empty = empty_set();
    let empty_map: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let cooldown = names(&["alpha", "bravo"]);
    let ctx = OrphanRecoveryContext {
        in_progress: &tasks,
        active_names: &empty,
        recently_stopped: &empty,
        attached_coworkers: &empty_map,
        channel_lead_names: &empty,
        spawn_failure_cooldown_names: &cooldown,
    };
    let result = decide_orphan_recovery(&ctx);
    assert!(
        result.is_none(),
        "should return None when all owners on cooldown"
    );
}
