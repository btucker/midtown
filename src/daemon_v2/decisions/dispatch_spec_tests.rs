//! Behavioral tests for v2-spec.md Section 2: Task Dispatch
//!
//! Each test maps to a specific SHALL requirement from the spec.

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::decisions::dispatch::{
    check_duplicate_workers, dispatch_pending_tasks, stop_completed_agents,
};
use crate::daemon_v2::decisions::health::{check_dead_workers, check_idle_workers};
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;

fn make_worker(proj: &mut Projections, id: &str, name: &str, task_id: &str) {
    proj.apply(&DomainEvent::AgentCreated {
        id: id.into(),
        name: name.into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some(task_id.into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: id.into(),
        pid: 1000,
        session_id: Some("sess-1".into()),
    });
}

fn make_task(proj: &mut Projections, id: &str, channel: &str) {
    proj.apply(&DomainEvent::TaskCreated {
        id: id.into(),
        subject: format!("Task {id}"),
        channel: channel.into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
}

fn spawn_task_ids(commands: &[Command]) -> Vec<Option<String>> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::SpawnAgent(cfg) => Some(cfg.task_id.clone()),
            _ => None,
        })
        .collect()
}

// ── Section 2.1: Spawning Workers ────────────────────────────────────────────

/// Spec 2.1: WHEN pending unblocked tasks exist AND fewer than max_in_progress tasks
/// are running THEN the system SHALL spawn a worker for each available slot
#[test]
fn spawns_workers_for_available_slots() {
    let mut proj = Projections::default();
    make_task(&mut proj, "t1", "main");
    make_task(&mut proj, "t2", "main");

    // Only 1 task in-progress, max is 3 → 2 slots available
    proj.apply(&DomainEvent::TaskCreated {
        id: "t-running".into(),
        subject: "Running task".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t-running".into(),
        agent_id: "existing-agent".into(),
    });

    let commands = dispatch_pending_tasks(&proj, 3);

    assert_eq!(
        commands.len(),
        2,
        "expected 2 spawns for 2 pending tasks with 2 slots, got {:?}",
        commands
    );
    let task_ids = spawn_task_ids(&commands);
    assert!(task_ids.contains(&Some("t1".into())), "should spawn for t1");
    assert!(task_ids.contains(&Some("t2".into())), "should spawn for t2");
}

/// Spec 2.1: WHEN fewer than max_in_progress tasks are running THEN do not exceed
/// max_in_progress by spawning more than the available slots
#[test]
fn respects_max_in_progress_cap() {
    let mut proj = Projections::default();
    // 3 tasks in-progress, 1 pending — limit is 3
    for i in 0..3 {
        let task_id = format!("running-{i}");
        make_task(&mut proj, &task_id, "main");
        proj.apply(&DomainEvent::TaskAssigned {
            task_id: task_id.clone(),
            agent_id: format!("agent-{i}"),
        });
    }
    make_task(&mut proj, "pending-1", "main");

    let commands = dispatch_pending_tasks(&proj, 3);

    assert!(
        commands.is_empty(),
        "expected no spawns when at max_in_progress, got {:?}",
        commands
    );
}

/// Spec 2.1: WHEN a task has no agent_type THEN the system SHALL use
/// midtown-code-author as default
#[test]
fn uses_default_agent_type_when_none_specified() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "No agent type".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });

    let commands = dispatch_pending_tasks(&proj, 3);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::SpawnAgent(cfg) if cfg.agent_type == "midtown-code-author"
        ),
        "expected midtown-code-author as default agent_type, got {:?}",
        commands[0]
    );
}

/// Spec 2.1: WHEN a task specifies agent_type THEN the system SHALL use that type
#[test]
fn uses_specified_agent_type() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Custom agent type".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: Some("midtown-code-reviewer".into()),
        icon: None,
    });

    let commands = dispatch_pending_tasks(&proj, 3);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::SpawnAgent(cfg) if cfg.agent_type == "midtown-code-reviewer"
        ),
        "expected midtown-code-reviewer agent_type, got {:?}",
        commands[0]
    );
}

/// Spec 2.1: WHEN a task is in a lead_driven channel THEN the system SHALL NOT
/// auto-dispatch it
#[test]
fn does_not_dispatch_lead_driven_channel_tasks() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::ChannelLeadDrivenSet {
        channel: "manual".into(),
        lead_driven: true,
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Lead-driven task".into(),
        channel: "manual".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });

    let commands = dispatch_pending_tasks(&proj, 5);

    assert!(
        commands.is_empty(),
        "expected no spawns for lead_driven channel, got {:?}",
        commands
    );
}

/// Spec 2.1: WHEN spawning a worker THEN the system SHALL generate a unique name,
/// random icon, and random color
#[test]
fn spawned_worker_has_unique_name_icon_and_color() {
    let mut proj = Projections::default();
    make_task(&mut proj, "t1", "main");
    make_task(&mut proj, "t2", "main");

    let commands = dispatch_pending_tasks(&proj, 3);

    assert_eq!(commands.len(), 2);
    let names: Vec<&str> = commands
        .iter()
        .filter_map(|c| match c {
            Command::SpawnAgent(cfg) => Some(cfg.name.as_str()),
            _ => None,
        })
        .collect();

    // Names must be unique
    let mut deduped = names.clone();
    deduped.dedup();
    assert_eq!(
        names.len(),
        deduped.len(),
        "spawned names should be unique: {:?}",
        names
    );

    // Each spawn must have an icon and color set
    for cmd in &commands {
        if let Command::SpawnAgent(cfg) = cmd {
            assert!(
                cfg.icon.is_some(),
                "spawn for {} should have an icon",
                cfg.name
            );
            assert!(
                cfg.color.is_some(),
                "spawn for {} should have a color",
                cfg.name
            );
        }
    }
}

// ── Section 2.2: Task Lifecycle ───────────────────────────────────────────────

/// Spec 2.2: WHEN a worker dies while its task is InProgress THEN the system SHALL
/// reset the task to Pending
#[test]
fn dead_worker_resets_task_to_pending() {
    let mut proj = Projections::default();
    make_task(&mut proj, "t1", "main");
    make_worker(&mut proj, "a1", "ghost-town", "t1");
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    proj.apply(&DomainEvent::AgentStopped {
        id: "a1".into(),
        reason: "process died".into(),
    });

    let commands = check_dead_workers(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::ResetTask { task_id } if task_id == "t1"),
        "expected ResetTask for t1, got {:?}",
        commands[0]
    );
}

/// Spec 2.2: WHEN two agents are assigned to the same task THEN the system SHALL
/// stop the older one
#[test]
fn duplicate_workers_stops_older_agent() {
    use chrono::{Duration, Utc};

    let mut proj = Projections::default();
    make_task(&mut proj, "t1", "main");

    // Create two workers for the same task
    proj.apply(&DomainEvent::AgentCreated {
        id: "old-agent".into(),
        name: "old-worker".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("t1".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "old-agent".into(),
        pid: 100,
        session_id: None,
    });
    proj.apply(&DomainEvent::AgentCreated {
        id: "new-agent".into(),
        name: "new-worker".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("t1".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "new-agent".into(),
        pid: 101,
        session_id: None,
    });

    // Make old-agent definitively older
    proj.agents.by_id.get_mut("old-agent").unwrap().started_at =
        Some(Utc::now() - Duration::minutes(10));
    proj.agents.by_id.get_mut("new-agent").unwrap().started_at = Some(Utc::now());

    let commands = check_duplicate_workers(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::StopAgent { id, reason }
                if id == "old-agent" && reason == "duplicate worker for task"
        ),
        "expected StopAgent for old-agent, got {:?}",
        commands[0]
    );
}

/// Spec 2.2: WHEN a worker has no task for more than 5 minutes THEN the system
/// SHALL stop it
#[test]
fn idle_worker_stopped_after_five_minutes() {
    use chrono::{Duration, Utc};

    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "idle-worker".into(),
        name: "quiet-river".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "idle-worker".into(),
        pid: 200,
        session_id: None,
    });

    // Back-date started_at to beyond the 5-minute idle threshold
    proj.agents.by_id.get_mut("idle-worker").unwrap().started_at =
        Some(Utc::now() - Duration::minutes(6));

    let commands = check_idle_workers(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::StopAgent { id, reason }
                if id == "idle-worker" && reason == "idle worker"
        ),
        "expected StopAgent for idle worker, got {:?}",
        commands[0]
    );
}

/// Spec 2.2: WHEN a worker has no task but was started less than 5 minutes ago
/// THEN the system SHALL NOT stop it yet
#[test]
fn recently_started_idle_worker_not_stopped() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "new-idle".into(),
        name: "fresh-brook".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "new-idle".into(),
        pid: 201,
        session_id: None,
    });
    // started_at defaults to Utc::now() — within the 5-minute window

    let commands = check_idle_workers(&proj);

    assert!(
        commands.is_empty(),
        "recently started idle worker should not be stopped, got {:?}",
        commands
    );
}

/// Spec 2.2: WHEN a running worker's task is Completed THEN the system SHALL stop
/// the worker
#[test]
fn worker_stopped_when_task_completed() {
    let mut proj = Projections::default();
    make_task(&mut proj, "t1", "main");
    make_worker(&mut proj, "a1", "calm-cedar", "t1");
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    proj.apply(&DomainEvent::TaskCompleted {
        task_id: "t1".into(),
    });

    let commands = stop_completed_agents(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::StopAgent { id, reason }
                if id == "a1" && reason == "task completed"
        ),
        "expected StopAgent for a1 with completed task, got {:?}",
        commands[0]
    );
}

/// Spec 2.2: WHEN a task declares blocked_by dependencies THEN the system SHALL
/// NOT dispatch it until all blockers are completed
#[test]
fn blocked_task_not_dispatched_until_blockers_complete() {
    let mut proj = Projections::default();

    // t1 is unblocked, t2 is blocked by t1
    make_task(&mut proj, "t1", "main");
    proj.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Blocked task".into(),
        channel: "main".into(),
        blocked_by: vec!["t1".into()],
        agent_type: None,
        icon: None,
    });

    let commands = dispatch_pending_tasks(&proj, 5);

    // Only t1 should be dispatched, t2 is blocked
    assert_eq!(
        commands.len(),
        1,
        "expected only 1 spawn for the unblocked task, got {:?}",
        commands
    );
    assert!(
        matches!(
            &commands[0],
            Command::SpawnAgent(cfg) if cfg.task_id.as_deref() == Some("t1")
        ),
        "expected spawn for t1 only, got {:?}",
        commands[0]
    );
}

/// Spec 2.2: WHEN all blockers complete (TaskUnblocked applied) THEN the blocked
/// task becomes eligible for dispatch
#[test]
fn task_dispatched_after_blocker_unblocked() {
    let mut proj = Projections::default();

    make_task(&mut proj, "t1", "main");
    proj.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Previously blocked".into(),
        channel: "main".into(),
        blocked_by: vec!["t1".into()],
        agent_type: None,
        icon: None,
    });

    // Complete t1 and unblock t2
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    proj.apply(&DomainEvent::TaskCompleted {
        task_id: "t1".into(),
    });
    proj.apply(&DomainEvent::TaskUnblocked {
        task_id: "t2".into(),
    });

    let commands = dispatch_pending_tasks(&proj, 5);

    let task_ids = spawn_task_ids(&commands);
    assert!(
        task_ids.contains(&Some("t2".into())),
        "t2 should be dispatched after t1 unblocks it, got {:?}",
        commands
    );
}
