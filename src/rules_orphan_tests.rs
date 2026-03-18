//! Tests for orphan recovery decision logic.

use std::collections::{HashMap, HashSet};

use super::{OrphanRecoveryContext, decide_orphan_recovery};

fn empty_set() -> HashSet<String> {
    HashSet::new()
}

fn empty_map() -> HashMap<String, chrono::DateTime<chrono::Utc>> {
    HashMap::new()
}

#[test]
fn skips_cooldown_blocked_owner_recovers_next_orphan() {
    // Two orphaned tasks: first owner ("park") is on spawn failure cooldown,
    // second owner ("vernon") is not. Should skip park and return vernon's task.
    let tasks = vec![
        (
            "10".to_string(),
            "park task".to_string(),
            "park".to_string(),
        ),
        (
            "20".to_string(),
            "vernon task".to_string(),
            "vernon".to_string(),
        ),
    ];
    let empty = empty_set();
    let cooldown_names: HashSet<String> = ["park".to_string()].into_iter().collect();
    let ctx = OrphanRecoveryContext {
        in_progress: &tasks,
        active_names: &empty,
        recently_stopped: &empty,
        attached_coworkers: &empty_map(),
        channel_lead_names: &empty,
        spawn_failure_cooldown_names: &cooldown_names,
    };
    let result = decide_orphan_recovery(&ctx);
    assert!(result.is_some(), "should recover the non-cooldown orphan");
    let recovery = result.unwrap();
    assert_eq!(recovery.task_id, "20");
    assert_eq!(recovery.owner, "vernon");
}

#[test]
fn all_owners_on_cooldown_returns_none() {
    // All orphan owners are on cooldown — should return None, not deadlock.
    let tasks = vec![
        (
            "10".to_string(),
            "park task".to_string(),
            "park".to_string(),
        ),
        (
            "20".to_string(),
            "vernon task".to_string(),
            "vernon".to_string(),
        ),
    ];
    let empty = empty_set();
    let cooldown_names: HashSet<String> = ["park".to_string(), "vernon".to_string()]
        .into_iter()
        .collect();
    let ctx = OrphanRecoveryContext {
        in_progress: &tasks,
        active_names: &empty,
        recently_stopped: &empty,
        attached_coworkers: &empty_map(),
        channel_lead_names: &empty,
        spawn_failure_cooldown_names: &cooldown_names,
    };
    let result = decide_orphan_recovery(&ctx);
    assert!(
        result.is_none(),
        "all owners on cooldown — nothing to recover"
    );
}
