//! Behavioral tests for v2-spec.md Section 1: Message Routing
//!
//! Each test maps to a specific SHALL requirement from the spec.

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::decisions::chat::route_message;
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;

fn make_lead(proj: &mut Projections, name: &str, channel: &str) -> String {
    make_lead_with_type(proj, name, channel, "midtown-project-lead")
}

fn make_channel_lead(proj: &mut Projections, name: &str, channel: &str) -> String {
    make_lead_with_type(proj, name, channel, "midtown-channel-lead")
}

fn make_lead_with_type(
    proj: &mut Projections,
    name: &str,
    channel: &str,
    agent_type: &str,
) -> String {
    let id = format!("lead-{name}");
    proj.apply(&DomainEvent::AgentCreated {
        id: id.clone(),
        name: name.into(),
        kind: AgentKind::Lead,
        agent_type: agent_type.into(),
        provider: Provider::ClaudeCode,
        channel: Some(channel.into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: id.clone(),
        pid: 1000,
        session_id: Some("sess-1".into()),
    });
    id
}

fn make_worker(proj: &mut Projections, name: &str, channel: &str, task_id: &str) -> String {
    let id = format!("worker-{name}");
    proj.apply(&DomainEvent::AgentCreated {
        id: id.clone(),
        name: name.into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some(channel.into()),
        task_id: Some(task_id.into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: id.clone(),
        pid: 2000,
        session_id: Some("sess-w".into()),
    });
    id
}

fn make_fork(proj: &mut Projections, name: &str, channel: &str, thread_id: &str) -> String {
    let id = format!("fork-{name}");
    proj.apply(&DomainEvent::AgentCreated {
        id: id.clone(),
        name: name.into(),
        kind: AgentKind::Fork,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some(channel.into()),
        task_id: None,
        bound_thread_id: Some(thread_id.into()),
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: id.clone(),
        pid: 3000,
        session_id: Some("sess-f".into()),
    });
    id
}

fn nudge_targets(commands: &[Command]) -> Vec<String> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::NudgeAgent { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

// ── Section 1.1: Thread Routing ──────────────────────────────────────────

/// Spec 1.1: WHEN a message is posted to a thread AND an agent is bound to that
/// thread THEN the system SHALL nudge that bound agent
#[test]
fn thread_reply_nudges_bound_agent() {
    let mut proj = Projections::default();
    let lead_id = make_lead(&mut proj, "main-lead", "main");
    let fork_id = make_fork(&mut proj, "fork-1", "main", "thread-abc");

    let cmds = route_message(&proj, "main", "user", "hello", Some("thread-abc"));
    let targets = nudge_targets(&cmds);

    assert!(targets.contains(&fork_id), "bound fork should be nudged");
    assert!(
        !targets.contains(&lead_id),
        "lead should NOT be nudged when fork handles the thread"
    );
}

/// Spec 1.1: WHEN a message is posted to a thread AND no agent is bound to that
/// thread THEN the system SHALL nudge the channel lead
#[test]
fn thread_reply_without_fork_nudges_lead() {
    let mut proj = Projections::default();
    let lead_id = make_lead(&mut proj, "main-lead", "main");

    let cmds = route_message(&proj, "main", "user", "hello", Some("thread-xyz"));
    let targets = nudge_targets(&cmds);

    assert!(
        targets.contains(&lead_id),
        "lead should be nudged when no fork for thread"
    );
}

/// Spec 1.1: WHEN a top-level message is posted THEN the system SHALL nudge the
/// channel lead
#[test]
fn top_level_message_nudges_lead() {
    let mut proj = Projections::default();
    let lead_id = make_lead(&mut proj, "main-lead", "main");

    let cmds = route_message(&proj, "main", "user", "hello everyone", None);
    let targets = nudge_targets(&cmds);

    assert!(
        targets.contains(&lead_id),
        "lead should be nudged on top-level message"
    );
}

// ── Section 1.2: @Mentions ───────────────────────────────────────────────

/// Spec 1.2: WHEN a message contains @agent-name THEN the system SHALL nudge
/// the named agent
#[test]
fn at_mention_nudges_named_agent() {
    let mut proj = Projections::default();
    let _lead_id = make_lead(&mut proj, "main-lead", "main");
    let worker_id = make_worker(&mut proj, "park", "main", "t1");

    let cmds = route_message(&proj, "main", "user", "hey @park can you check this?", None);
    let targets = nudge_targets(&cmds);

    assert!(
        targets.contains(&worker_id),
        "@park should nudge the worker"
    );
}

/// Spec 1.2: WHEN @all in main channel THEN nudge ALL channel leads AND ALL
/// agents bound to in-progress tasks across ALL channels, excluding sender
#[test]
fn at_all_in_main_channel_nudges_all_leads_and_task_agents() {
    let mut proj = Projections::default();
    let main_lead_id = make_lead(&mut proj, "main-lead", "main");
    let web_lead_id = make_channel_lead(&mut proj, "web-lead", "web");
    let worker_id = make_worker(&mut proj, "park", "web", "t1");
    // Worker needs an in-progress task
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "web".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: worker_id.clone(),
    });

    // @all from main channel should reach ALL leads + ALL in-progress task agents
    let cmds = route_message(&proj, "main", "user", "@all heads up!", None);
    let targets = nudge_targets(&cmds);

    assert!(
        targets.contains(&main_lead_id),
        "@all should nudge main lead"
    );
    assert!(
        targets.contains(&web_lead_id),
        "@all in main should nudge OTHER channel leads too"
    );
    assert!(
        targets.contains(&worker_id),
        "@all in main should nudge task-bound workers across channels"
    );
}

/// Spec 1.2: WHEN @all in topic channel THEN nudge channel lead AND agents
/// in that channel bound to in-progress tasks, excluding sender
#[test]
fn at_all_in_topic_channel_nudges_local_lead_and_task_agents() {
    let mut proj = Projections::default();
    let _main_lead_id = make_lead(&mut proj, "main-lead", "main");
    let web_lead_id = make_channel_lead(&mut proj, "web-lead", "web");
    let web_worker_id = make_worker(&mut proj, "park", "web", "t1");
    let _other_worker_id = make_worker(&mut proj, "amsterdam", "ops", "t2");
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "web".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: web_worker_id.clone(),
    });

    // @all from #web should only reach web's lead + web's task workers
    let cmds = route_message(&proj, "web", "user", "@all update please", None);
    let targets = nudge_targets(&cmds);

    assert!(
        targets.contains(&web_lead_id),
        "@all in topic should nudge topic lead"
    );
    assert!(
        targets.contains(&web_worker_id),
        "@all in topic should nudge local task-bound worker"
    );
    // Should NOT nudge main lead or ops worker
    assert!(
        !targets.iter().any(|t| t.contains("main-lead")),
        "@all in topic should NOT nudge other channel leads"
    );
    assert!(
        !targets.iter().any(|t| t.contains("amsterdam")),
        "@all in topic should NOT nudge workers from other channels"
    );
}

/// Spec 1.2: WHEN a message contains @lead THEN the system SHALL nudge the
/// channel lead
#[test]
fn at_lead_nudges_channel_lead() {
    let mut proj = Projections::default();
    let lead_id = make_lead(&mut proj, "main-lead", "main");

    let cmds = route_message(&proj, "main", "user", "@lead what's the status?", None);
    let targets = nudge_targets(&cmds);

    assert!(targets.contains(&lead_id), "@lead should nudge lead");
}

/// Spec 1.2: WHEN a @mention refers to an unknown agent THEN the system SHALL
/// NOT emit a nudge
#[test]
fn unknown_mention_no_nudge() {
    let mut proj = Projections::default();
    let _lead_id = make_lead(&mut proj, "main-lead", "main");

    let cmds = route_message(&proj, "main", "user", "@nonexistent hello", None);
    let targets = nudge_targets(&cmds);

    // Only the lead should be nudged (top-level message rule), not @nonexistent
    assert_eq!(targets.len(), 1, "only lead nudged, not unknown agent");
}

/// Spec 1.2: WHEN a @mention contains trailing punctuation THEN the system SHALL
/// strip it before lookup
#[test]
fn mention_with_trailing_punctuation() {
    let mut proj = Projections::default();
    let _lead_id = make_lead(&mut proj, "main-lead", "main");
    let worker_id = make_worker(&mut proj, "park", "main", "t1");

    let cmds = route_message(&proj, "main", "user", "@park, can you look?", None);
    let targets = nudge_targets(&cmds);

    assert!(
        targets.contains(&worker_id),
        "@park, (with comma) should still nudge park"
    );
}

/// Spec 1.2: WHEN a message contains @channel-name THEN the system SHALL
/// nudge that channel's lead (cross-channel)
#[test]
fn at_channel_name_nudges_that_channels_lead() {
    let mut proj = Projections::default();
    let _main_lead = make_lead(&mut proj, "main-lead", "main");
    let web_lead_id = make_channel_lead(&mut proj, "web-lead", "web");

    // Message in #main mentions @web → should nudge web's lead
    let cmds = route_message(&proj, "main", "user", "hey @web can you check this?", None);
    let targets = nudge_targets(&cmds);

    assert!(
        targets.contains(&web_lead_id),
        "@web should nudge web channel's lead"
    );
}

/// Spec 1.2: WHEN a message contains @channel-name AND the channel exists in
/// ChannelIndex but has no agents yet THEN the system SHALL demand-spawn a lead
#[test]
fn at_channel_name_spawns_lead_for_known_channel_without_agents() {
    let mut proj = Projections::default();
    let _main_lead = make_lead(&mut proj, "main-lead", "main");

    // Register "docs" in ChannelIndex without any agents
    proj.apply(&DomainEvent::MessagePosted {
        id: "m1".into(),
        channel: "docs".into(),
        sender: "system".into(),
        content: "channel created".into(),
        thread_id: None,
    });

    // @docs should trigger a lead spawn (channel exists but has no agents)
    let cmds = route_message(&proj, "main", "user", "hey @docs check this", None);

    let spawn_leads: Vec<_> = cmds
        .iter()
        .filter(|c| {
            matches!(c, Command::SpawnAgent(cfg) if cfg.kind == AgentKind::Lead && cfg.channel.as_deref() == Some("docs"))
        })
        .collect();
    assert_eq!(
        spawn_leads.len(),
        1,
        "@docs should spawn a lead for docs channel, got {:?}",
        cmds
    );
}

/// Spec 5.1: WHEN a user messages a channel AND no lead exists THEN the
/// system SHALL spawn one
#[test]
fn message_to_leadless_channel_spawns_lead() {
    let mut proj = Projections::default();
    // No lead for "web" channel — just a worker
    let _worker = make_worker(&mut proj, "park", "web", "t1");

    let cmds = route_message(&proj, "web", "user", "hello web channel", None);

    // Should include a SpawnAgent for a lead
    let spawn_leads: Vec<_> = cmds
        .iter()
        .filter(|c| matches!(c, Command::SpawnAgent(cfg) if cfg.kind == AgentKind::Lead))
        .collect();
    assert_eq!(
        spawn_leads.len(),
        1,
        "should spawn a lead for leadless channel, got {:?}",
        cmds
    );
}

/// Spec 5.1: WHEN a user messages a channel AND a lead already exists THEN
/// no additional lead SHALL be spawned
#[test]
fn message_to_channel_with_lead_does_not_spawn() {
    let mut proj = Projections::default();
    let _lead = make_channel_lead(&mut proj, "web-lead", "web");

    let cmds = route_message(&proj, "web", "user", "hello", None);

    let spawn_leads: Vec<_> = cmds
        .iter()
        .filter(|c| matches!(c, Command::SpawnAgent(cfg) if cfg.kind == AgentKind::Lead))
        .collect();
    assert!(
        spawn_leads.is_empty(),
        "should NOT spawn lead when one exists"
    );
}

// ── Section 1.3: Task References ─────────────────────────────────────────

/// Spec 1.3: WHEN a message contains !N THEN the system SHALL nudge the agent
/// assigned to task N
#[test]
fn task_ref_nudges_assigned_agent() {
    let mut proj = Projections::default();
    let _lead_id = make_lead(&mut proj, "main-lead", "main");
    let worker_id = make_worker(&mut proj, "park", "main", "42");

    proj.apply(&DomainEvent::TaskCreated {
        id: "42".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });

    let cmds = route_message(&proj, "main", "user", "check !42 please", None);
    let targets = nudge_targets(&cmds);

    assert!(
        targets.contains(&worker_id),
        "!42 should nudge assigned worker"
    );
}

/// Spec 1.3: WHEN a task reference has no assigned agent THEN the system SHALL
/// NOT emit a nudge
#[test]
fn task_ref_no_assigned_agent() {
    let mut proj = Projections::default();
    let lead_id = make_lead(&mut proj, "main-lead", "main");

    proj.apply(&DomainEvent::TaskCreated {
        id: "99".into(),
        subject: "Unassigned".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });

    let cmds = route_message(&proj, "main", "user", "what about !99?", None);
    let targets = nudge_targets(&cmds);

    // Only lead nudge (top-level), no task nudge
    assert_eq!(targets, vec![lead_id], "only lead, not unassigned task");
}

/// Spec 1.3: WHEN a message contains !N THEN the system SHALL nudge agents
/// assigned to all descendant tasks of N
#[test]
fn task_ref_nudges_descendant_agents() {
    let mut proj = Projections::default();
    let _lead_id = make_lead(&mut proj, "main-lead", "main");

    // Create parent task 10 with child task 11
    proj.apply(&DomainEvent::TaskCreated {
        id: "10".into(),
        subject: "Parent".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "11".into(),
        subject: "Child".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: Some("10".into()),
    });

    let parent_worker = make_worker(&mut proj, "alpha", "main", "10");
    let child_worker = make_worker(&mut proj, "beta", "main", "11");

    let cmds = route_message(&proj, "main", "user", "check !10 please", None);
    let targets = nudge_targets(&cmds);

    assert!(
        targets.contains(&parent_worker),
        "!10 should nudge parent worker"
    );
    assert!(
        targets.contains(&child_worker),
        "!10 should also nudge child worker (descendant)"
    );
}

/// Spec 1.3: WHEN a task is created with a parent field THEN the system SHALL
/// record the parent-child relationship
#[test]
fn task_parent_child_recorded() {
    let mut proj = Projections::default();

    proj.apply(&DomainEvent::TaskCreated {
        id: "100".into(),
        subject: "Parent".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "101".into(),
        subject: "Child A".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: Some("100".into()),
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "102".into(),
        subject: "Child B".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: Some("100".into()),
    });

    let descendants = proj.work.descendants_of("100");
    assert_eq!(
        descendants.len(),
        2,
        "task 100 should have 2 descendants, got {:?}",
        descendants
    );
    assert!(descendants.contains(&"101".to_string()));
    assert!(descendants.contains(&"102".to_string()));
}

// ── Section 1.4: Nudge Invariants ────────────────────────────────────────

/// Spec 1.4: WHEN the sender is the same as the nudge target THEN the system
/// SHALL suppress the nudge
#[test]
fn self_nudge_suppressed() {
    let mut proj = Projections::default();
    let _lead_id = make_lead(&mut proj, "main-lead", "main");

    // Lead posts a message — should NOT nudge itself
    let cmds = route_message(&proj, "main", "main-lead", "status update", None);
    let targets = nudge_targets(&cmds);

    assert!(targets.is_empty(), "lead should not nudge itself");
}

/// Spec 1.4: WHEN multiple routing rules match the same agent THEN the system
/// SHALL nudge it exactly once
#[test]
fn dedup_multiple_matches() {
    let mut proj = Projections::default();
    let lead_id = make_lead(&mut proj, "main-lead", "main");

    // Message that matches both top-level (→ lead) and @lead (→ lead)
    let cmds = route_message(&proj, "main", "user", "@lead hello @main-lead", None);
    let targets = nudge_targets(&cmds);

    let lead_count = targets.iter().filter(|t| *t == &lead_id).count();
    assert_eq!(
        lead_count, 1,
        "lead should be nudged exactly once, not {lead_count}"
    );
}

// ── Section 4.5: Fork Resume on Thread Reply ────────────────────────────

/// Spec 4.5: WHEN a fork session stops THEN its thread binding SHALL persist
/// AND any subsequent nudge to the thread SHALL target the stopped fork
/// (executor handles resume via NudgeAction::ResumeAndDeliver)
#[test]
fn stopped_fork_still_targeted_by_thread_reply() {
    let mut proj = Projections::default();
    let _lead_id = make_lead(&mut proj, "main-lead", "main");
    let fork_id = make_fork(&mut proj, "research", "main", "thread-123");

    // Stop the fork
    proj.apply(&DomainEvent::AgentStopped {
        id: fork_id.clone(),
        reason: "idle".into(),
    });

    // Thread binding should persist after stop (spec 6.1)
    assert!(
        proj.agents.by_thread.contains_key("thread-123"),
        "thread binding should persist after fork stops"
    );

    // Thread reply should still target the stopped fork (not the lead)
    let cmds = route_message(&proj, "main", "user", "any update?", Some("thread-123"));
    let targets = nudge_targets(&cmds);

    assert!(
        targets.contains(&fork_id),
        "thread reply should target the stopped fork for resume, got {:?}",
        targets
    );
}
