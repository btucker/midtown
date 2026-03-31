use chrono::{Duration, Utc};

use super::*;
use crate::daemon_v2::events::{AgentKind, Provider};
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::projections::agents::{Agent, AgentIndex};

fn make_agent(
    id: &str,
    name: &str,
    kind: AgentKind,
    stopped_at: Option<chrono::DateTime<Utc>>,
) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        kind,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: None,
        session_id: None,
        pid: None,
        started_at: None,
        stopped_at,
        icon: None,
        color: None,
        reported_state: None,
        state_reported_at: None,
        last_output_at: None,
        gc: false,
    }
}

fn proj_with_agents(agents: Vec<Agent>, running_ids: Vec<&str>) -> Projections {
    let mut proj = Projections::default();
    for agent in agents {
        let id = agent.id.clone();
        proj.agents.by_id.insert(id.clone(), agent);
    }
    for id in running_ids {
        proj.agents.running.insert(id.into());
    }
    proj
}

#[test]
fn create_dm_channel_name_prefixes_dm() {
    assert_eq!(create_dm_channel_name("ghost-town"), "dm-ghost-town");
    assert_eq!(create_dm_channel_name("worker-abc"), "dm-worker-abc");
}

#[test]
fn gc_removes_old_stopped_agents() {
    let old_stopped_at = Some(Utc::now() - Duration::hours(25));
    let worker = make_agent("w1", "ghost-town", AgentKind::Worker, old_stopped_at);
    let proj = proj_with_agents(vec![worker], vec![]);

    let ids = garbage_collect_agents(&proj);
    assert_eq!(ids, vec!["w1"]);
}

#[test]
fn gc_keeps_recently_stopped_agents() {
    // Stopped 1 hour ago — should not be collected
    let recent_stopped_at = Some(Utc::now() - Duration::hours(1));
    let worker = make_agent("w1", "ghost-town", AgentKind::Worker, recent_stopped_at);
    let proj = proj_with_agents(vec![worker], vec![]);

    let ids = garbage_collect_agents(&proj);
    assert!(ids.is_empty(), "expected no GC, got {:?}", ids);
}

#[test]
fn gc_keeps_running_agents() {
    // Running agent — should not be collected even if stopped_at is old
    let old_stopped_at = Some(Utc::now() - Duration::hours(25));
    let worker = make_agent("w1", "ghost-town", AgentKind::Worker, old_stopped_at);
    let proj = proj_with_agents(vec![worker], vec!["w1"]);

    let ids = garbage_collect_agents(&proj);
    assert!(
        ids.is_empty(),
        "expected no GC for running agent, got {:?}",
        ids
    );
}

#[test]
fn gc_keeps_leads() {
    // Stopped lead — should not be collected (may be resumed)
    let old_stopped_at = Some(Utc::now() - Duration::hours(25));
    let lead = make_agent("l1", "main", AgentKind::Lead, old_stopped_at);
    let proj = proj_with_agents(vec![lead], vec![]);

    let ids = garbage_collect_agents(&proj);
    assert!(ids.is_empty(), "expected no GC for lead, got {:?}", ids);
}

#[test]
fn gc_keeps_agents_with_no_stopped_at() {
    // Agent with no stopped_at (never stopped) should not be collected
    let worker = make_agent("w1", "ghost-town", AgentKind::Worker, None);
    let proj = proj_with_agents(vec![worker], vec![]);

    let ids = garbage_collect_agents(&proj);
    assert!(
        ids.is_empty(),
        "expected no GC for agent without stopped_at, got {:?}",
        ids
    );
}

#[test]
fn gc_keeps_forks() {
    // Fork agents stopped long ago — Forks are not Leads, so they ARE collected
    // (Forks are thread-bound research sessions; they don't need to be resumed)
    let old_stopped_at = Some(Utc::now() - Duration::hours(25));
    let fork = make_agent("f1", "fork-abc", AgentKind::Fork, old_stopped_at);
    let proj = proj_with_agents(vec![fork], vec![]);

    let ids = garbage_collect_agents(&proj);
    assert_eq!(ids, vec!["f1"], "expected fork to be GC'd");
}

#[test]
fn gc_decision_returns_garbage_collect_commands() {
    let old_stopped_at = Some(Utc::now() - Duration::hours(25));
    let worker = make_agent("w1", "ghost-town", AgentKind::Worker, old_stopped_at);
    let proj = proj_with_agents(vec![worker], vec![]);

    let commands = gc_decision(&proj, "main");
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], crate::daemon_v2::decisions::Command::GarbageCollect { agent_id } if agent_id == "w1"),
        "expected GarbageCollect for w1, got {:?}",
        commands[0]
    );
}

#[test]
fn agent_index_gc_removes_from_all_indexes() {
    use crate::daemon_v2::events::DomainEvent;

    let mut idx = AgentIndex::default();

    // Create and start an agent with a task and channel
    idx.apply(&DomainEvent::AgentCreated {
        id: "w1".into(),
        name: "ghost-town".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("task-1".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "w1".into(),
        pid: 123,
        session_id: None,
    });
    idx.apply(&DomainEvent::AgentStopped {
        id: "w1".into(),
        reason: "done".into(),
    });

    // Verify it's in the indexes before GC
    assert!(idx.by_id.contains_key("w1"));
    assert!(idx.by_name.contains_key("ghost-town"));
    assert!(idx.by_task.contains_key("task-1"));
    assert!(idx.by_channel.contains_key("main"));

    // Apply GC event
    idx.apply(&DomainEvent::AgentGarbageCollected { id: "w1".into() });

    // Spec 6.1: record preserved in by_id but marked gc=true
    assert!(idx.by_id.contains_key("w1"), "by_id should preserve record");
    assert!(idx.by_id.get("w1").unwrap().gc, "gc flag should be true");

    // Removed from all routing indexes
    assert!(
        !idx.by_name.contains_key("ghost-town"),
        "by_name should be empty"
    );
    assert!(
        !idx.by_task.contains_key("task-1"),
        "by_task should be empty"
    );
    assert!(
        !idx.by_channel.contains_key("main"),
        "by_channel should be empty (last agent)"
    );
    assert!(!idx.running.contains("w1"), "running should be empty");
}

// Worktree cleanup is handled event-driven in DaemonV2::handle_worktree_cleanup()
// — no periodic decision function needed.
