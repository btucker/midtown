use crate::daemon::state::DaemonPersistentState;

#[allow(clippy::field_reassign_with_default)]
fn test_ps() -> DaemonPersistentState {
    let mut ps = DaemonPersistentState::default();
    ps.tick_dir_key = "test-repo".into();
    ps.tick_project_name = "test-repo".into();
    ps.tick_default_channel = "test-repo".into();
    ps.tick_default_branch = "main".into();
    ps.tick_now = chrono::Utc::now();
    ps
}

#[test]
fn test_stale_worktree_emits_nudge() {
    let mut ps = test_ps();
    ps.tick_stale_lead_worktrees
        .insert("daemon-core".to_string());
    ps.channel_lead_sessions
        .insert("daemon-core".to_string(), "session-123".to_string());

    let effects = super::check_channel_lead_worktree_freshness(&ps);

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
    let mut ps = test_ps();
    // stale_lead_worktrees is empty by default
    ps.channel_lead_sessions
        .insert("daemon-core".to_string(), "session-123".to_string());

    let effects = super::check_channel_lead_worktree_freshness(&ps);

    assert!(
        effects.is_empty(),
        "Expected no effects for fresh worktree, got {:?}",
        effects
    );
}

#[test]
fn test_cooldown_prevents_re_nudge() {
    let mut ps = test_ps();
    ps.tick_stale_lead_worktrees
        .insert("daemon-core".to_string());
    ps.channel_lead_sessions
        .insert("daemon-core".to_string(), "session-123".to_string());
    // Channel is on cooldown
    ps.tick_lead_worktree_freshness_cooldown_channels
        .insert("daemon-core".to_string());

    let effects = super::check_channel_lead_worktree_freshness(&ps);

    assert!(
        effects.is_empty(),
        "Expected no effects when cooldown is active, got {:?}",
        effects
    );
}
