use super::super::effects::Effect;
use super::*;
use crate::rules::{CooldownTracker, MentionAction};
use std::time::Duration;

#[test]
fn mention_nudge_produces_nudge_effect() {
    let action = MentionAction::Nudge {
        name: "lexington".to_string(),
        message: "lead said (msg-42): @lexington check this".to_string(),
    };
    let effects = mention_action_to_effects(action, "lexington", "test-repo");

    assert_eq!(effects.len(), 1);
    assert!(
        matches!(&effects[0], Effect::NudgeCoworker { name, message }
            if name == "lexington" && message.contains("msg-42")),
        "NudgeCoworker message must include the message ID in parentheses"
    );
}

#[test]
fn mention_spawn_produces_spawn_with_callbacks() {
    let action = MentionAction::Spawn {
        name: "park".to_string(),
        message: "lead said (msg-99): @park fix the bug".to_string(),
    };
    let effects = mention_action_to_effects(action, "park", "test-repo");

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
            // Success and failure callbacks must post to OPS_CHANNEL (not default channel)
            assert!(
                matches!(&on_success[0], Effect::PostToChannel { channel: Some(ch), message, .. }
                    if ch == OPS_CHANNEL && message.contains("park") && message.contains("@mention")),
                "Success callback should post to OPS_CHANNEL mentioning park and @mention"
            );
            assert!(
                matches!(&on_failure[0], Effect::PostToChannel { channel: Some(ch), .. }
                    if ch == OPS_CHANNEL),
                "Failure callback should post to OPS_CHANNEL"
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
    let effects = mention_action_to_effects(action, "lexington", "test-repo");
    assert!(
        effects.is_empty(),
        "Skip (non dev-limit) should produce no effects"
    );
}

#[test]
fn mention_skip_dev_limit_posts_to_ops_channel() {
    let action = MentionAction::Skip {
        reason: "Cannot spawn amsterdam: dev limit reached".to_string(),
    };
    let effects = mention_action_to_effects(action, "amsterdam", "test-repo");

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel {
            channel, message, ..
        } => {
            assert_eq!(
                channel.as_deref(),
                Some(OPS_CHANNEL),
                "Dev-limit notice must go to OPS_CHANNEL"
            );
            assert!(message.contains("amsterdam"), "Should mention the coworker");
            assert!(
                message.contains("dev coworkers limit"),
                "Should explain the limit"
            );
        }
        _ => panic!("Expected PostToChannel for dev limit, got {:?}", effects[0]),
    }
}

/// The deduplication key `chat_mention_{name}` must block a second nudge for the
/// same (name, msg_id) pair. This tests the CooldownTracker wiring as used by
/// `route_mentions`: the combined check+record path inside a single lock scope.
#[test]
fn mention_dedup_wiring_blocks_second_nudge_for_same_message() {
    let mut cooldowns = CooldownTracker::new();
    let msg_id = "msg-dedupe-001";
    let key = "chat_mention_amsterdam";

    // Simulate the wiring in route_mentions: check then record in one scope.
    let first = {
        if cooldowns.check(key, msg_id, Duration::from_secs(3600)) {
            cooldowns.record(key, msg_id);
            true
        } else {
            false
        }
    };
    assert!(first, "First call must be allowed");

    // Second call with the same msg_id must be blocked.
    let second = {
        if cooldowns.check(key, msg_id, Duration::from_secs(3600)) {
            cooldowns.record(key, msg_id);
            true
        } else {
            false
        }
    };
    assert!(
        !second,
        "Second call with same msg_id must be blocked by deduplication"
    );
}

/// Deduplication must be per-recipient: blocking amsterdam for a message must
/// not block broadway for the same message (separate CooldownTracker keys).
#[test]
fn mention_dedup_wiring_is_per_recipient() {
    let mut cooldowns = CooldownTracker::new();
    let msg_id = "msg-broadcast-999";

    // Record for amsterdam.
    cooldowns.record("chat_mention_amsterdam", msg_id);

    // broadway with the same msg_id must still be allowed.
    let broadway_allowed =
        cooldowns.check("chat_mention_broadway", msg_id, Duration::from_secs(3600));
    assert!(
        broadway_allowed,
        "Different recipient must not be blocked by another's dedup record"
    );
}

// Deduplication tests for the CooldownTracker-based nudge guards added to
// route_mentions and route_at_all.

#[test]
fn chat_mention_cooldown_blocks_duplicate_message() {
    let mut tracker = CooldownTracker::new();
    let msg_id = "msg-abc-123";
    let rule = "chat_mention_lexington";

    // First check: no prior record, should nudge
    assert!(
        tracker.check(rule, msg_id, Duration::from_secs(3600)),
        "First check should allow nudge"
    );
    tracker.record(rule, msg_id);

    // Same message ID: should be blocked
    assert!(
        !tracker.check(rule, msg_id, Duration::from_secs(3600)),
        "Duplicate message ID should be blocked"
    );
}

#[test]
fn chat_mention_cooldown_allows_different_message_ids() {
    let mut tracker = CooldownTracker::new();
    let rule = "chat_mention_lexington";

    tracker.record(rule, "msg-1");

    // Different message ID should still be allowed
    assert!(
        tracker.check(rule, "msg-2", Duration::from_secs(3600)),
        "Different message ID should be allowed"
    );
}

#[test]
fn chat_at_all_cooldown_is_per_recipient() {
    let mut tracker = CooldownTracker::new();
    let msg_id = "msg-broadcast-xyz";

    // Record nudge for lead
    tracker.record("chat_at_all_lead", msg_id);

    // Lead should be blocked
    assert!(
        !tracker.check("chat_at_all_lead", msg_id, Duration::from_secs(3600)),
        "Lead should be blocked after record"
    );

    // Coworker with same message ID should still be allowed (separate rule key)
    assert!(
        tracker.check("chat_at_all_lexington", msg_id, Duration::from_secs(3600)),
        "Different coworker should not be blocked by lead's record"
    );
}

#[test]
fn chat_at_all_coworker_cooldown_blocks_duplicate() {
    let mut tracker = CooldownTracker::new();
    let msg_id = "msg-broadcast-xyz";
    let rule = "chat_at_all_park";

    assert!(tracker.check(rule, msg_id, Duration::from_secs(3600)));
    tracker.record(rule, msg_id);
    assert!(
        !tracker.check(rule, msg_id, Duration::from_secs(3600)),
        "Same message should be blocked after record"
    );
}
