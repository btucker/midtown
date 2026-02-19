use super::super::effects::Effect;
use super::*;
use crate::rules::MentionAction;

#[test]
fn mention_nudge_produces_nudge_effect() {
    let action = MentionAction::Nudge {
        name: "lexington".to_string(),
        message: "lead said: @lexington check this".to_string(),
    };
    let effects = mention_action_to_effects(action, "lexington", "test-repo", None);

    assert_eq!(effects.len(), 1);
    assert!(
        matches!(&effects[0], Effect::NudgeCoworker { name, .. } if name == "lexington"),
        "Expected NudgeCoworker for lexington"
    );
}

#[test]
fn mention_spawn_produces_spawn_with_callbacks() {
    let action = MentionAction::Spawn {
        name: "park".to_string(),
        message: "lead said: @park fix the bug".to_string(),
    };
    let effects = mention_action_to_effects(action, "park", "test-repo", None);

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::SpawnCoworkerWithCallbacks {
            config,
            on_success,
            on_failure,
        } => {
            assert_eq!(config.name, "park");
            assert!(!on_success.is_empty(), "Should have success callback");
            assert!(!on_failure.is_empty(), "Should have failure callback");
            // Success callback should post to channel
            assert!(
                matches!(&on_success[0], Effect::PostToChannel { message, .. }
                    if message.contains("park") && message.contains("@mention")),
                "Success callback should mention park and @mention"
            );
        }
        _ => panic!("Expected SpawnCoworkerWithCallbacks, got {:?}", effects[0]),
    }
}

#[test]
fn mention_skip_produces_no_effects() {
    let action = MentionAction::Skip {
        reason: "lexington is already active, no need to spawn".to_string(),
    };
    let effects = mention_action_to_effects(action, "lexington", "test-repo", None);
    assert!(
        effects.is_empty(),
        "Skip (non dev-limit) should produce no effects"
    );
}

#[test]
fn mention_skip_dev_limit_posts_to_channel() {
    let action = MentionAction::Skip {
        reason: "Cannot spawn amsterdam: dev limit reached".to_string(),
    };
    let effects = mention_action_to_effects(action, "amsterdam", "test-repo", None);

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel { message, .. } => {
            assert!(message.contains("amsterdam"), "Should mention the coworker");
            assert!(
                message.contains("dev coworkers limit"),
                "Should explain the limit"
            );
        }
        _ => panic!("Expected PostToChannel for dev limit, got {:?}", effects[0]),
    }
}

#[test]
fn mention_action_to_effects_includes_session_id() {
    let action = MentionAction::Nudge {
        name: "lexington".to_string(),
        message: "lead said: @lexington check this".to_string(),
    };
    let effects = mention_action_to_effects(
        action,
        "lexington",
        "test-repo",
        Some("sess-abc-123".to_string()),
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::NudgeCoworker {
            name, session_id, ..
        } => {
            assert_eq!(name, "lexington");
            assert_eq!(
                session_id.as_deref(),
                Some("sess-abc-123"),
                "NudgeCoworker should include the provided session_id"
            );
        }
        _ => panic!("Expected NudgeCoworker, got {:?}", effects[0]),
    }
}

#[test]
fn mention_action_to_effects_no_session_id() {
    let action = MentionAction::Nudge {
        name: "lexington".to_string(),
        message: "lead said: @lexington check this".to_string(),
    };
    let effects = mention_action_to_effects(action, "lexington", "test-repo", None);

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::NudgeCoworker {
            name, session_id, ..
        } => {
            assert_eq!(name, "lexington");
            assert_eq!(
                session_id.as_deref(),
                None,
                "NudgeCoworker should have None session_id when not provided"
            );
        }
        _ => panic!("Expected NudgeCoworker, got {:?}", effects[0]),
    }
}
