use super::snapshot;

#[test]
fn test_stale_worktree_emits_nudge() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.stale_channel_lead_worktrees
        .insert("daemon-core".to_string());
    snap.channel_lead_sessions
        .insert("daemon-core".to_string(), "session-123".to_string());

    let effects = super::check_channel_lead_worktree_freshness(&snap);

    // Should emit a NudgeChannelLead and a RecordCooldown
    assert!(
        effects.len() == 2,
        "Expected 2 effects, got {}: {:?}",
        effects.len(),
        effects
    );

    // Check NudgeChannelLead
    let has_nudge = effects.iter().any(|e| {
        matches!(
            e,
            super::Effect::NudgeChannelLead {
                channel_name, ..
            } if channel_name == "daemon-core"
        )
    });
    assert!(has_nudge, "Expected NudgeChannelLead for daemon-core");

    // Check RecordCooldown
    let has_cooldown = effects.iter().any(|e| {
        matches!(
            e,
            super::Effect::RecordCooldown {
                category,
                key,
            } if category == "lead_worktree_freshness" && key == "daemon-core"
        )
    });
    assert!(
        has_cooldown,
        "Expected RecordCooldown for lead_worktree_freshness"
    );
}

#[test]
fn test_fresh_worktree_emits_nothing() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    // stale_channel_lead_worktrees is empty by default
    snap.channel_lead_sessions
        .insert("daemon-core".to_string(), "session-123".to_string());

    let effects = super::check_channel_lead_worktree_freshness(&snap);

    assert!(
        effects.is_empty(),
        "Expected no effects for fresh worktree, got {:?}",
        effects
    );
}

#[test]
fn test_cooldown_prevents_re_nudge() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.stale_channel_lead_worktrees
        .insert("daemon-core".to_string());
    snap.channel_lead_sessions
        .insert("daemon-core".to_string(), "session-123".to_string());
    // Channel is on cooldown
    snap.lead_worktree_freshness_cooldown_channels
        .insert("daemon-core".to_string());

    let effects = super::check_channel_lead_worktree_freshness(&snap);

    assert!(
        effects.is_empty(),
        "Expected no effects when cooldown is active, got {:?}",
        effects
    );
}
