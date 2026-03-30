//! Behavioral tests for v2-spec.md Section 4: Agent Lifecycle (pure projection tests)
//!
//! Each test maps to a specific SHALL requirement from the spec.
//! Only projection and decision logic is tested — no I/O, no async.

use chrono::{Duration, Utc};

use crate::daemon_v2::decisions::lifecycle::garbage_collect_agents;
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::projections::agents::{Agent, AgentIndex};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn make_stopped_worker(id: &str, name: &str, hours_ago: i64) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: None,
        session_id: None,
        pid: None,
        started_at: None,
        stopped_at: Some(Utc::now() - Duration::hours(hours_ago)),
        icon: None,
        color: None,
        gc: false,
    }
}

fn make_stopped_lead(id: &str, name: &str, hours_ago: i64) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-project-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: None,
        session_id: None,
        pid: None,
        started_at: None,
        stopped_at: Some(Utc::now() - Duration::hours(hours_ago)),
        icon: None,
        color: None,
        gc: false,
    }
}

fn proj_with_agents(agents: Vec<Agent>) -> Projections {
    let mut proj = Projections::default();
    for agent in agents {
        let id = agent.id.clone();
        proj.agents.by_id.insert(id, agent);
    }
    proj
}

// ── Section 4.1: Spawning ───────────────────────────────────────────────────

/// Spec 4.1: WHEN spawning succeeds THEN AgentCreated and AgentStarted events
/// SHALL be emitted (projection side — events produce correct state)
#[test]
fn spawn_events_create_running_agent() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "w1".into(),
        name: "swift-river".into(),
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
        id: "w1".into(),
        pid: 5000,
        session_id: Some("sess-1".into()),
    });

    let agent = proj.agents.by_id.get("w1").unwrap();
    assert_eq!(agent.pid, Some(5000));
    assert_eq!(agent.session_id.as_deref(), Some("sess-1"));
    assert!(agent.started_at.is_some());
    assert!(proj.agents.running.contains("w1"));
    assert_eq!(proj.agents.by_task.get("t1"), Some(&"w1".to_string()));
}

/// Spec 4.1: WHEN an agent is spawned AND its output is not bound to a channel
/// or thread THEN the system SHALL auto-create a DM channel dm-{agent_name}
#[test]
fn dm_channel_name_generated_correctly() {
    let name = crate::daemon_v2::decisions::lifecycle::create_dm_channel_name("swift-river");
    assert_eq!(name, "dm-swift-river");
}

// ── Section 4.2: Stopping ───────────────────────────────────────────────────

/// Spec 4.2: WHEN StopAgent is executed THEN AgentStopped emitted, but the
/// session ID SHALL be preserved for potential resume
#[test]
fn stop_preserves_session_id_for_resume() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "w1".into(),
        name: "calm-brook".into(),
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
        id: "w1".into(),
        pid: 5000,
        session_id: Some("sess-preserve".into()),
    });
    proj.apply(&DomainEvent::AgentStopped {
        id: "w1".into(),
        reason: "manual stop".into(),
    });

    let agent = proj.agents.by_id.get("w1").unwrap();
    assert!(!proj.agents.running.contains("w1"), "should not be running");
    assert_eq!(
        agent.session_id.as_deref(),
        Some("sess-preserve"),
        "session_id should be preserved after stop for potential resume"
    );
    assert!(agent.stopped_at.is_some());
}

/// Spec 4.2: WHEN an agent process exits (detected by try_wait) THEN
/// AgentStopped SHALL be emitted
#[test]
fn process_exit_emits_agent_stopped() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "w1".into(),
        name: "bold-hawk".into(),
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
        id: "w1".into(),
        pid: 5000,
        session_id: None,
    });
    // Simulate try_wait detecting process exit
    proj.apply(&DomainEvent::AgentStopped {
        id: "w1".into(),
        reason: "process exited".into(),
    });

    assert!(!proj.agents.running.contains("w1"));
    assert!(proj.agents.by_id.get("w1").unwrap().stopped_at.is_some());
}

// ── Section 4.3: Resuming ───────────────────────────────────────────────────

/// Spec 4.3: WHEN resume succeeds THEN AgentResumed SHALL be emitted with the
/// new PID AND started_at SHALL be reset to now
#[test]
fn resume_resets_started_at_and_updates_pid() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "w1".into(),
        name: "keen-falcon".into(),
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
        id: "w1".into(),
        pid: 5000,
        session_id: Some("sess-1".into()),
    });

    let first_started = proj.agents.by_id.get("w1").unwrap().started_at;

    proj.apply(&DomainEvent::AgentStopped {
        id: "w1".into(),
        reason: "crashed".into(),
    });

    // Small delay to ensure time moves (usually not needed, but just in case)
    proj.apply(&DomainEvent::AgentResumed {
        id: "w1".into(),
        pid: 6000,
    });

    let agent = proj.agents.by_id.get("w1").unwrap();
    assert_eq!(agent.pid, Some(6000), "pid should be updated");
    assert!(agent.stopped_at.is_none(), "stopped_at should be cleared");
    assert!(agent.started_at.is_some(), "started_at should be set");
    assert!(
        agent.started_at >= first_started,
        "started_at should be >= first start"
    );
    assert!(
        proj.agents.running.contains("w1"),
        "should be running again"
    );
}

// ── Section 4.4: Garbage Collection ──────────────────────────────────────────

/// Spec 4.4: WHEN an agent has been stopped for more than 24 hours AND is not a
/// Lead THEN it SHALL be garbage-collected
#[test]
fn gc_collects_worker_stopped_over_24h() {
    let worker = make_stopped_worker("w1", "old-worker", 25);
    let proj = proj_with_agents(vec![worker]);

    let ids = garbage_collect_agents(&proj);

    assert_eq!(ids, vec!["w1"], "worker stopped 25h ago should be GC'd");
}

/// Spec 4.4: WHEN an agent has been stopped for less than 24 hours THEN it SHALL
/// NOT be garbage-collected
#[test]
fn gc_does_not_collect_recently_stopped_worker() {
    let worker = make_stopped_worker("w1", "recent-worker", 23);
    let proj = proj_with_agents(vec![worker]);

    let ids = garbage_collect_agents(&proj);

    assert!(
        ids.is_empty(),
        "worker stopped 23h ago should NOT be GC'd, got {:?}",
        ids
    );
}

/// Spec 4.4: WHEN an agent is a Lead AND stopped for more than 24 hours THEN it
/// SHALL NOT be garbage-collected (leads may be resumed)
#[test]
fn gc_does_not_collect_leads() {
    let lead = make_stopped_lead("l1", "main", 48);
    let proj = proj_with_agents(vec![lead]);

    let ids = garbage_collect_agents(&proj);

    assert!(
        ids.is_empty(),
        "lead stopped 48h ago should NOT be GC'd (leads may be resumed), got {:?}",
        ids
    );
}

/// Spec 4.4: WHEN an agent is garbage-collected THEN it SHALL be marked as GC'd,
/// excluded from routing indexes, but record preserved in by_id
#[test]
fn gc_event_removes_agent_from_all_indexes() {
    let mut idx = AgentIndex::default();

    // Create a fork agent bound to a thread, with a task and channel
    idx.apply(&DomainEvent::AgentCreated {
        id: "w1".into(),
        name: "storm-peak".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("backend".into()),
        task_id: Some("task-7".into()),
        bound_thread_id: Some("thread-gc".into()),
        icon: None,
        color: None,
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "w1".into(),
        pid: 500,
        session_id: None,
    });
    idx.apply(&DomainEvent::AgentStopped {
        id: "w1".into(),
        reason: "done".into(),
    });

    // Confirm the agent is present in all indexes before GC
    assert!(idx.by_id.contains_key("w1"));
    assert!(idx.by_name.contains_key("storm-peak"));
    assert!(idx.by_task.contains_key("task-7"));
    assert!(
        idx.by_channel
            .get("backend")
            .is_some_and(|v| v.contains(&"w1".to_string()))
    );
    assert!(idx.by_thread.contains_key("thread-gc"));

    // Apply GC event
    idx.apply(&DomainEvent::AgentGarbageCollected { id: "w1".into() });

    // Record preserved but marked as GC'd (spec 6.1)
    assert!(
        idx.by_id.contains_key("w1"),
        "by_id should preserve GC'd record"
    );
    assert!(idx.by_id.get("w1").unwrap().gc, "gc flag should be true");

    // Excluded from all routing indexes
    assert!(
        !idx.by_name.contains_key("storm-peak"),
        "by_name should not contain storm-peak"
    );
    assert!(
        !idx.by_task.contains_key("task-7"),
        "by_task should not contain task-7"
    );
    assert!(
        !idx.by_channel
            .get("backend")
            .is_some_and(|v| v.contains(&"w1".to_string())),
        "by_channel should not contain w1"
    );
    assert!(
        !idx.by_thread.contains_key("thread-gc"),
        "by_thread should not contain thread-gc after GC"
    );
    assert!(!idx.running.contains("w1"), "running should not contain w1");
}

// ── Section 4.5: Fork Sessions ────────────────────────────────────────────────

/// Spec 4.5: WHEN a fork session spawns THEN it SHALL be bound to a thread via
/// bound_thread_id
#[test]
fn fork_created_with_bound_thread_id() {
    let mut proj = Projections::default();

    proj.apply(&DomainEvent::AgentCreated {
        id: "fork-1".into(),
        name: "coral-drift".into(),
        kind: AgentKind::Fork,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: Some("thread-xyz".into()),
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "fork-1".into(),
        pid: 4000,
        session_id: Some("sess-fork".into()),
    });

    let agent = proj.agents.by_id.get("fork-1").expect("fork should exist");
    assert_eq!(
        agent.bound_thread_id,
        Some("thread-xyz".into()),
        "fork should have bound_thread_id set"
    );
    assert!(
        proj.agents.by_thread.contains_key("thread-xyz"),
        "by_thread index should contain the fork's thread"
    );
}

/// Spec 4.5: WHEN a fork session stops THEN its thread binding SHALL persist
/// (NOT cleared) so it can be resumed for future thread activity
#[test]
fn fork_thread_binding_persists_after_stop() {
    let mut idx = AgentIndex::default();

    idx.apply(&DomainEvent::AgentCreated {
        id: "fork-1".into(),
        name: "silver-mist".into(),
        kind: AgentKind::Fork,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: Some("thread-persist".into()),
        icon: None,
        color: None,
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "fork-1".into(),
        pid: 4001,
        session_id: Some("sess-fork-2".into()),
    });
    idx.apply(&DomainEvent::AgentStopped {
        id: "fork-1".into(),
        reason: "idle".into(),
    });

    // Thread binding must survive the stop
    let agent = idx.by_id.get("fork-1").expect("fork should still exist");
    assert_eq!(
        agent.bound_thread_id,
        Some("thread-persist".into()),
        "bound_thread_id should NOT be cleared when fork stops"
    );
    assert!(
        idx.by_thread.contains_key("thread-persist"),
        "by_thread index should still map the thread to the stopped fork"
    );
    assert!(
        !idx.running.contains("fork-1"),
        "fork should be removed from running set on stop"
    );
}

/// Spec 4.5: WHEN a fork exists for a thread AND a session.fork request arrives
/// THEN the existing fork SHALL be returned (tested via projection lookup)
#[test]
fn fork_lookup_returns_existing_fork_for_thread() {
    let mut proj = Projections::default();

    proj.apply(&DomainEvent::AgentCreated {
        id: "fork-existing".into(),
        name: "jade-creek".into(),
        kind: AgentKind::Fork,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: Some("thread-lookup".into()),
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "fork-existing".into(),
        pid: 4002,
        session_id: Some("sess-fork-existing".into()),
    });

    // The fork_for_thread lookup should find the existing running fork
    let found = proj
        .agents
        .fork_for_thread("thread-lookup")
        .expect("fork_for_thread should return the existing fork");

    assert_eq!(
        found.id, "fork-existing",
        "should return the existing fork id"
    );
    assert_eq!(
        found.bound_thread_id,
        Some("thread-lookup".into()),
        "found fork should be bound to the requested thread"
    );
}
