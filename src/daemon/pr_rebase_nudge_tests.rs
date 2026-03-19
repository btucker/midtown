use super::collect_merge_rebase_nudge_effects;
use crate::daemon::effects::Effect;
use crate::daemon::state::{DaemonPersistentState, SessionRecord};

/// Helper: set up a coworker with an open PR in persistent state.
fn add_coworker_with_open_pr(
    ps: &mut DaemonPersistentState,
    name: &str,
    session_id: &str,
    pr_number: u64,
) {
    ps.sessions.insert(
        session_id.to_string(),
        SessionRecord {
            session_id: session_id.to_string(),
            name: name.to_string(),
            pr_number: Some(pr_number),
            ..Default::default()
        },
    );
    ps.tick_name_session_map
        .insert(name.to_string(), session_id.to_string());
    ps.tick_open_prs
        .push(serde_json::json!({"number": pr_number, "title": "test"}));
}

#[test]
fn nudges_coworkers_with_open_prs_when_pr_merges() {
    let mut ps = DaemonPersistentState::default();
    ps.tick_merged_pr_numbers.insert(100);
    add_coworker_with_open_pr(&mut ps, "lexington", "session-1", 200);
    add_coworker_with_open_pr(&mut ps, "park", "session-2", 201);

    let effects = collect_merge_rebase_nudge_effects(&ps);

    let nudge_names: Vec<String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::NudgeCoworkerByName { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(nudge_names.len(), 2);
    assert!(nudge_names.contains(&"lexington".to_string()));
    assert!(nudge_names.contains(&"park".to_string()));

    // Each nudge should be paired with a RecordCooldown
    let cooldown_keys: Vec<String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::RecordCooldown { category, key } if category == "merge_rebase_nudge" => {
                Some(key.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(cooldown_keys.len(), 2);

    // Merged PR numbers should be marked as processed
    let processed_pr_keys: Vec<String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::RecordCooldown { category, key } if category == "merge_rebase_pr_processed" => {
                Some(key.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(processed_pr_keys.len(), 1);
    assert!(processed_pr_keys.contains(&"100".to_string()));
}

#[test]
fn does_not_nudge_coworker_whose_pr_merged() {
    let mut ps = DaemonPersistentState::default();
    ps.tick_merged_pr_numbers.insert(100);
    // lexington has open PR #200 AND merged PR #100
    add_coworker_with_open_pr(&mut ps, "lexington", "session-1", 200);
    // Also give lexington a merged PR
    ps.sessions.get_mut("session-1").unwrap().pr_number = Some(100);
    // Remove the open PR data for lexington and re-add with merged PR number
    ps.tick_open_prs.clear();
    ps.tick_open_prs
        .push(serde_json::json!({"number": 100, "title": "merged pr"}));

    add_coworker_with_open_pr(&mut ps, "park", "session-2", 201);

    let effects = collect_merge_rebase_nudge_effects(&ps);

    let nudge_names: Vec<String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::NudgeCoworkerByName { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(nudge_names.len(), 1);
    assert_eq!(nudge_names[0], "park");
}

#[test]
fn does_not_nudge_coworkers_on_cooldown() {
    let mut ps = DaemonPersistentState::default();
    ps.tick_merged_pr_numbers.insert(100);
    add_coworker_with_open_pr(&mut ps, "lexington", "session-1", 200);
    add_coworker_with_open_pr(&mut ps, "park", "session-2", 201);
    // lexington is on cooldown
    ps.tick_merge_rebase_nudge_cooldown_names
        .insert("lexington".to_string());

    let effects = collect_merge_rebase_nudge_effects(&ps);

    let nudge_names: Vec<String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::NudgeCoworkerByName { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(nudge_names.len(), 1);
    assert_eq!(nudge_names[0], "park");
}

#[test]
fn does_not_nudge_coworkers_without_sessions() {
    let mut ps = DaemonPersistentState::default();
    ps.tick_merged_pr_numbers.insert(100);
    // lexington has a session with open PR but no tick_name_session_map entry
    ps.sessions.insert(
        "session-1".to_string(),
        SessionRecord {
            session_id: "session-1".to_string(),
            name: "lexington".to_string(),
            pr_number: Some(200),
            ..Default::default()
        },
    );
    ps.tick_open_prs
        .push(serde_json::json!({"number": 200, "title": "test"}));
    // No tick_name_session_map entry for lexington

    // park has a proper session mapping
    add_coworker_with_open_pr(&mut ps, "park", "session-2", 201);

    let effects = collect_merge_rebase_nudge_effects(&ps);

    let nudge_names: Vec<String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::NudgeCoworkerByName { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(nudge_names.len(), 1);
    assert_eq!(nudge_names[0], "park");
}

#[test]
fn no_nudges_when_no_prs_merged() {
    let mut ps = DaemonPersistentState::default();
    add_coworker_with_open_pr(&mut ps, "lexington", "session-1", 200);

    let effects = collect_merge_rebase_nudge_effects(&ps);
    assert!(effects.is_empty());
}

#[test]
fn nudge_message_contains_rebase_guidance() {
    let mut ps = DaemonPersistentState::default();
    ps.tick_merged_pr_numbers.insert(42);
    add_coworker_with_open_pr(&mut ps, "lexington", "session-1", 200);

    let effects = collect_merge_rebase_nudge_effects(&ps);

    let message = effects
        .iter()
        .find_map(|e| match e {
            Effect::NudgeCoworkerByName { message, .. } => Some(message.clone()),
            _ => None,
        })
        .expect("should have a nudge");

    assert!(
        message.contains("#42"),
        "should mention the merged PR number"
    );
    assert!(
        message.contains("git rebase origin/main"),
        "should include rebase command"
    );
    assert!(
        message.contains("MUST re-read"),
        "should include re-read guidance"
    );
    assert!(
        message.contains("stale versions"),
        "should warn about stale context"
    );
}

#[test]
fn skips_already_processed_merged_prs() {
    let mut ps = DaemonPersistentState::default();
    ps.tick_merged_pr_numbers.insert(100);
    ps.tick_rebase_nudge_processed_prs.insert(100);
    add_coworker_with_open_pr(&mut ps, "lexington", "session-1", 200);

    let effects = collect_merge_rebase_nudge_effects(&ps);

    assert!(
        effects.is_empty(),
        "should not nudge for already-processed merged PRs"
    );
}

#[test]
fn only_nudges_for_new_merged_prs_not_processed_ones() {
    let mut ps = DaemonPersistentState::default();
    ps.tick_merged_pr_numbers.insert(100);
    ps.tick_merged_pr_numbers.insert(200);
    ps.tick_rebase_nudge_processed_prs.insert(100);
    add_coworker_with_open_pr(&mut ps, "lexington", "session-1", 300);

    let effects = collect_merge_rebase_nudge_effects(&ps);

    let nudge_messages: Vec<String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::NudgeCoworkerByName { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(nudge_messages.len(), 1);
    assert!(
        nudge_messages[0].contains("#200"),
        "should mention new PR #200"
    );
    assert!(
        !nudge_messages[0].contains("#100"),
        "should not mention already-processed PR #100"
    );

    let processed_pr_keys: Vec<String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::RecordCooldown { category, key } if category == "merge_rebase_pr_processed" => {
                Some(key.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(processed_pr_keys.len(), 1);
    assert_eq!(processed_pr_keys[0], "200");
}
