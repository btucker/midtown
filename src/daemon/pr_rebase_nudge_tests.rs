use super::collect_merge_rebase_nudge_effects;
use crate::daemon::effects::Effect;
use crate::daemon::snapshot::minimal_snapshot_for_test;

#[test]
fn nudges_coworkers_with_open_prs_when_pr_merges() {
    let mut snap = minimal_snapshot_for_test();
    snap.pr.merged_pr_numbers.insert(100);
    snap.pr
        .coworkers_with_open_prs
        .insert("lexington".to_string());
    snap.pr.coworkers_with_open_prs.insert("park".to_string());
    snap.name_session_map
        .insert("lexington".to_string(), "session-1".to_string());
    snap.name_session_map
        .insert("park".to_string(), "session-2".to_string());

    let effects = collect_merge_rebase_nudge_effects(&snap);

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
}

#[test]
fn does_not_nudge_coworker_whose_pr_merged() {
    let mut snap = minimal_snapshot_for_test();
    snap.pr.merged_pr_numbers.insert(100);
    snap.pr
        .coworkers_with_open_prs
        .insert("lexington".to_string());
    snap.pr.coworkers_with_open_prs.insert("park".to_string());
    snap.pr
        .coworkers_with_merged_prs
        .insert("lexington".to_string());
    snap.name_session_map
        .insert("lexington".to_string(), "session-1".to_string());
    snap.name_session_map
        .insert("park".to_string(), "session-2".to_string());

    let effects = collect_merge_rebase_nudge_effects(&snap);

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
    let mut snap = minimal_snapshot_for_test();
    snap.pr.merged_pr_numbers.insert(100);
    snap.pr
        .coworkers_with_open_prs
        .insert("lexington".to_string());
    snap.pr.coworkers_with_open_prs.insert("park".to_string());
    snap.name_session_map
        .insert("lexington".to_string(), "session-1".to_string());
    snap.name_session_map
        .insert("park".to_string(), "session-2".to_string());
    // lexington is on cooldown
    snap.merge_rebase_nudge_cooldown_names
        .insert("lexington".to_string());

    let effects = collect_merge_rebase_nudge_effects(&snap);

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
    let mut snap = minimal_snapshot_for_test();
    snap.pr.merged_pr_numbers.insert(100);
    snap.pr
        .coworkers_with_open_prs
        .insert("lexington".to_string());
    snap.pr.coworkers_with_open_prs.insert("park".to_string());
    // Only park has a session
    snap.name_session_map
        .insert("park".to_string(), "session-2".to_string());

    let effects = collect_merge_rebase_nudge_effects(&snap);

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
    let mut snap = minimal_snapshot_for_test();
    // No merged PRs
    snap.pr
        .coworkers_with_open_prs
        .insert("lexington".to_string());
    snap.name_session_map
        .insert("lexington".to_string(), "session-1".to_string());

    let effects = collect_merge_rebase_nudge_effects(&snap);
    assert!(effects.is_empty());
}

#[test]
fn nudge_message_contains_rebase_guidance() {
    let mut snap = minimal_snapshot_for_test();
    snap.pr.merged_pr_numbers.insert(42);
    snap.pr
        .coworkers_with_open_prs
        .insert("lexington".to_string());
    snap.name_session_map
        .insert("lexington".to_string(), "session-1".to_string());

    let effects = collect_merge_rebase_nudge_effects(&snap);

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
