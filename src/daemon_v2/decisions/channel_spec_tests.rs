//! Behavioral tests for v2-spec.md Section 5: Channel Management
//!
//! Each test maps to a specific SHALL requirement from the spec.
//! Pure — no I/O, no async.

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::decisions::health::ensure_channel_leads_alive;
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn make_projections(events: &[DomainEvent]) -> Projections {
    let mut proj = Projections::default();
    proj.apply_all(events);
    proj
}

fn spawn_configs(commands: &[Command]) -> Vec<&crate::daemon_v2::decisions::SpawnConfig> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::SpawnAgent(cfg) => Some(cfg),
            _ => None,
        })
        .collect()
}

// ── Section 5.1: Channel Leads ────────────────────────────────────────────────

/// Spec 5.1: WHEN a non-archived channel has no running lead THEN the system
/// SHALL spawn one
#[test]
fn non_archived_channel_without_lead_spawns_one() {
    let events = vec![DomainEvent::MessagePosted {
        id: "msg-1".into(),
        channel: "teamwork".into(),
        sender: "alice".into(),
        content: "hello".into(),
        thread_id: None,
        tool_data: None,
    }];

    let proj = make_projections(&events);
    let commands = ensure_channel_leads_alive(&proj, "main");

    let configs = spawn_configs(&commands);
    // "main" (default, always) + "teamwork" (non-archived, no lead)
    assert_eq!(
        configs.len(),
        2,
        "should spawn leads for both main and teamwork, got {:?}",
        commands
    );
    let channels: Vec<Option<&str>> = configs.iter().map(|c| c.channel.as_deref()).collect();
    assert!(
        channels.contains(&Some("teamwork")),
        "teamwork should have a lead spawned"
    );
}

/// Spec 5.1: WHEN spawning a lead for the default channel THEN agent_type SHALL
/// be midtown-project-lead
#[test]
fn default_channel_lead_uses_project_lead_agent_type() {
    let proj = Projections::default();
    let commands = ensure_channel_leads_alive(&proj, "main");

    let configs = spawn_configs(&commands);
    assert_eq!(
        configs.len(),
        1,
        "exactly one spawn for empty default channel"
    );

    let cfg = &configs[0];
    assert_eq!(cfg.channel.as_deref(), Some("main"));
    assert_eq!(
        cfg.agent_type, "midtown-project-lead",
        "default channel lead should be midtown-project-lead, got {}",
        cfg.agent_type
    );
}

/// Spec 5.1: WHEN spawning a lead for a topic channel THEN agent_type SHALL be
/// midtown-channel-lead
#[test]
fn topic_channel_lead_uses_channel_lead_agent_type() {
    let events = vec![DomainEvent::MessagePosted {
        id: "msg-1".into(),
        channel: "design".into(),
        sender: "bob".into(),
        content: "hi".into(),
        thread_id: None,
        tool_data: None,
    }];

    let proj = make_projections(&events);
    let commands = ensure_channel_leads_alive(&proj, "main");

    let design_cfg = spawn_configs(&commands)
        .into_iter()
        .find(|c| c.channel.as_deref() == Some("design"))
        .expect("should spawn lead for design channel");

    assert_eq!(
        design_cfg.agent_type, "midtown-channel-lead",
        "topic channel lead should be midtown-channel-lead, got {}",
        design_cfg.agent_type
    );
}

/// Spec 5.1: WHEN a channel has a directory setting THEN the lead's working_dir
/// SHALL be set to that subdirectory
#[test]
fn channel_with_directory_sets_lead_working_dir() {
    let events = vec![
        DomainEvent::MessagePosted {
            id: "msg-1".into(),
            channel: "auth".into(),
            sender: "alice".into(),
            content: "hi".into(),
            thread_id: None,
            tool_data: None,
        },
        DomainEvent::ChannelDirectorySet {
            channel: "auth".into(),
            directory: Some("packages/auth".into()),
        },
    ];

    let proj = make_projections(&events);
    let commands = ensure_channel_leads_alive(&proj, "main");

    let auth_cfg = spawn_configs(&commands)
        .into_iter()
        .find(|c| c.channel.as_deref() == Some("auth"))
        .expect("should spawn lead for auth channel");

    assert_eq!(
        auth_cfg.working_dir.as_deref(),
        Some("packages/auth"),
        "lead working_dir should match the channel directory setting, got {:?}",
        auth_cfg.working_dir
    );
}

/// Spec 5.1: WHEN a channel is archived THEN the system SHALL NOT spawn a lead
#[test]
fn archived_channel_does_not_spawn_lead() {
    let events = vec![DomainEvent::MessagePosted {
        id: "msg-1".into(),
        channel: "legacy".into(),
        sender: "alice".into(),
        content: "old".into(),
        thread_id: None,
        tool_data: None,
    }];

    let mut proj = make_projections(&events);
    proj.channels.channels.get_mut("legacy").unwrap().archived = true;

    let commands = ensure_channel_leads_alive(&proj, "main");
    let channels: Vec<Option<&str>> = spawn_configs(&commands)
        .iter()
        .map(|c| c.channel.as_deref())
        .collect();

    assert!(
        !channels.contains(&Some("legacy")),
        "archived channel should not get a lead spawned"
    );
}

// ── Section 5.2: Channel Settings ────────────────────────────────────────────

/// Spec 5.2: WHEN ChannelLeadDrivenSet is applied THEN lead_driven flag SHALL
/// be updated in the projection
#[test]
fn channel_lead_driven_set_updates_projection() {
    let mut proj = Projections::default();

    // Initially lead_driven is false
    assert!(
        !proj.channels.is_lead_driven("ops"),
        "channel should not be lead_driven initially"
    );

    proj.apply(&DomainEvent::ChannelLeadDrivenSet {
        channel: "ops".into(),
        lead_driven: true,
    });

    assert!(
        proj.channels.is_lead_driven("ops"),
        "channel should be lead_driven after ChannelLeadDrivenSet"
    );
}

/// Spec 5.2: WHEN lead_driven is set to true THEN is_lead_driven returns true
/// for that channel (used by dispatch to skip auto-dispatch)
#[test]
fn lead_driven_true_reflected_in_projection() {
    let mut proj = Projections::default();

    proj.apply(&DomainEvent::ChannelLeadDrivenSet {
        channel: "product".into(),
        lead_driven: true,
    });

    assert!(
        proj.channels.is_lead_driven("product"),
        "is_lead_driven should return true after setting lead_driven=true"
    );
}

/// Spec 5.2: WHEN ChannelLeadDrivenSet is applied with false THEN lead_driven
/// flag SHALL be cleared
#[test]
fn channel_lead_driven_set_to_false_clears_flag() {
    let mut proj = Projections::default();

    proj.apply(&DomainEvent::ChannelLeadDrivenSet {
        channel: "main".into(),
        lead_driven: true,
    });
    assert!(proj.channels.is_lead_driven("main"));

    proj.apply(&DomainEvent::ChannelLeadDrivenSet {
        channel: "main".into(),
        lead_driven: false,
    });
    assert!(
        !proj.channels.is_lead_driven("main"),
        "lead_driven should be cleared after setting to false"
    );
}

/// Spec 5.2: WHEN ChannelDirectorySet is applied THEN directory SHALL be updated
/// in the projection
#[test]
fn channel_directory_set_updates_projection() {
    let mut proj = Projections::default();

    assert!(
        proj.channels.channel_directory("frontend").is_none(),
        "channel directory should be None initially"
    );

    proj.apply(&DomainEvent::ChannelDirectorySet {
        channel: "frontend".into(),
        directory: Some("packages/web".into()),
    });

    assert_eq!(
        proj.channels.channel_directory("frontend"),
        Some("packages/web"),
        "channel_directory should return the set path"
    );
}

/// Spec 5.2: WHEN ChannelDirectorySet is applied with None THEN directory SHALL
/// be cleared
#[test]
fn channel_directory_set_to_none_clears_directory() {
    let mut proj = Projections::default();

    proj.apply(&DomainEvent::ChannelDirectorySet {
        channel: "api".into(),
        directory: Some("packages/api".into()),
    });
    assert_eq!(proj.channels.channel_directory("api"), Some("packages/api"));

    proj.apply(&DomainEvent::ChannelDirectorySet {
        channel: "api".into(),
        directory: None,
    });
    assert!(
        proj.channels.channel_directory("api").is_none(),
        "directory should be None after clearing"
    );
}
