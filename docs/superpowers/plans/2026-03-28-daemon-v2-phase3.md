# Daemon v2 Phase 3: Real Agent Spawning and Task Dispatch

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the v2 daemon to spawn real Claude Code sessions, monitor their health, dispatch tasks to agents, and prove it works end-to-end with real Claude sessions using existing OAuth tokens.

**Architecture:** The `DaemonV2` struct gets a `sessions` map holding live `HeadlessSession` handles. The executor's `SpawnAgent` command calls `HeadlessSession::spawn()` via the existing `LaunchConfig → HeadlessConfig` bridge. A background task per session drains stdout events. `PollProcessHealth` uses `try_wait()` to detect exits, emitting `AgentStopped` events. Task dispatch decisions match pending tasks to idle agents.

**Tech Stack:** Rust, tokio, existing `HeadlessSession`/`LaunchConfig`/`ProjectPaths`

**Depends on:** Phase 2 (event loop, scheduler, executor skeleton)

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src/daemon_v2/daemon.rs` | Modify | Add sessions map, spawn/stop/health-check logic |
| `src/daemon_v2/executor/mod.rs` | Modify | Wire SpawnAgent, StopAgent, PollProcessHealth to real I/O |
| `src/daemon_v2/executor/spawn.rs` | Modify | Add spawn_agent() and stop_agent() async functions |
| `src/daemon_v2/decisions/dispatch.rs` | Create | Task dispatch decision functions |
| `src/daemon_v2/decisions/dispatch_tests.rs` | Create | Tests for dispatch decisions |
| `src/daemon_v2/decisions/mod.rs` | Modify | Add dispatch module |
| `tests/daemon_v2_e2e.rs` | Modify | Add real Claude session E2E tests |

---

### Task 1: Dispatch decision functions

**Files:**
- Create: `src/daemon_v2/decisions/dispatch.rs`
- Create: `src/daemon_v2/decisions/dispatch_tests.rs`
- Modify: `src/daemon_v2/decisions/mod.rs`

- [ ] **Step 1: Create src/daemon_v2/decisions/dispatch_tests.rs**

```rust
use super::*;
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;

#[test]
fn dispatches_pending_task_when_no_agents() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
    });

    let commands = dispatch::dispatch_pending_tasks(&proj, 3);
    // Should spawn an agent for the pending task
    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], Command::SpawnAgent(config) if config.task_id == Some("t1".into())));
}

#[test]
fn respects_max_in_progress_limit() {
    let mut proj = Projections::default();
    // Create 3 in-progress tasks
    for i in 0..3 {
        let id = format!("t{i}");
        proj.apply(&DomainEvent::TaskCreated {
            id: id.clone(), subject: format!("Task {i}"), channel: "main".into(), blocked_by: vec![],
        });
        proj.apply(&DomainEvent::TaskAssigned { task_id: id, agent_id: format!("a{i}") });
    }
    // One more pending
    proj.apply(&DomainEvent::TaskCreated {
        id: "t3".into(), subject: "Pending".into(), channel: "main".into(), blocked_by: vec![],
    });

    let commands = dispatch::dispatch_pending_tasks(&proj, 3);
    // At limit — should not spawn
    assert!(commands.is_empty());
}

#[test]
fn skips_blocked_tasks() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(), subject: "First".into(), channel: "main".into(), blocked_by: vec![],
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "t2".into(), subject: "Blocked".into(), channel: "main".into(), blocked_by: vec!["t1".into()],
    });

    let commands = dispatch::dispatch_pending_tasks(&proj, 5);
    // Only t1 should be dispatched, not blocked t2
    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], Command::SpawnAgent(config) if config.task_id == Some("t1".into())));
}

#[test]
fn stops_agents_for_completed_tasks() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "a1".into(), name: "worker-1".into(), kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(), provider: Provider::ClaudeCode,
        channel: Some("main".into()), task_id: Some("t1".into()),
    });
    proj.apply(&DomainEvent::AgentStarted { id: "a1".into(), pid: 1234 });
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(), subject: "Done task".into(), channel: "main".into(), blocked_by: vec![],
    });
    proj.apply(&DomainEvent::TaskAssigned { task_id: "t1".into(), agent_id: "a1".into() });
    proj.apply(&DomainEvent::TaskCompleted { task_id: "t1".into() });

    let commands = dispatch::stop_completed_agents(&proj);
    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], Command::StopAgent { id, .. } if id == "a1"));
}
```

- [ ] **Step 2: Create src/daemon_v2/decisions/dispatch.rs**

```rust
#[path = "dispatch_tests.rs"]
#[cfg(test)]
mod tests;

use crate::daemon_v2::decisions::{Command, SpawnConfig};
use crate::daemon_v2::events::{AgentKind, Provider, TaskStatus};
use crate::daemon_v2::projections::Projections;

/// Spawn agents for pending unblocked tasks, up to the max in-progress limit.
pub fn dispatch_pending_tasks(proj: &Projections, max_in_progress: usize) -> Vec<Command> {
    let current_in_progress = proj.work.in_progress_tasks.len();
    if current_in_progress >= max_in_progress {
        return vec![];
    }

    let slots = max_in_progress - current_in_progress;
    let pending = proj.work.pending_unblocked();

    pending
        .into_iter()
        .take(slots)
        .filter_map(|task_id| {
            let task = proj.work.tasks.get(task_id)?;
            Some(Command::SpawnAgent(SpawnConfig {
                name: format!("worker-{task_id}"),
                kind: AgentKind::Worker,
                agent_type: task.agent_type.clone().unwrap_or_else(|| "midtown-code-author".into()),
                provider: Provider::ClaudeCode,
                channel: Some(task.channel.clone()),
                task_id: Some(task_id.clone()),
                initial_prompt: Some(task.subject.clone()),
                working_dir: None,
                model: None,
            }))
        })
        .collect()
}

/// Stop running agents whose tasks have completed.
pub fn stop_completed_agents(proj: &Projections) -> Vec<Command> {
    proj.agents
        .by_id
        .values()
        .filter(|agent| {
            agent.kind == AgentKind::Worker
                && proj.agents.running.contains(&agent.id)
                && agent.task_id.as_ref().map_or(false, |tid| {
                    proj.work.tasks.get(tid).map_or(false, |t| t.status == TaskStatus::Completed)
                })
        })
        .map(|agent| Command::StopAgent {
            id: agent.id.clone(),
            reason: "task completed".into(),
        })
        .collect()
}
```

- [ ] **Step 3: Add dispatch to decisions/mod.rs**

Add `pub mod dispatch;` to `src/daemon_v2/decisions/mod.rs`.

- [ ] **Step 4: Verify tests pass**

Run: `cargo test --lib daemon_v2::decisions 2>&1 | tail -10`
Expected: All 8 decision tests pass (4 health + 4 dispatch).

- [ ] **Step 5: Commit**

```bash
git add src/daemon_v2/decisions/
git commit -m "feat(daemon-v2): add task dispatch decision functions"
```

---

### Task 2: Wire executor to real HeadlessSession spawning

**Files:**
- Modify: `src/daemon_v2/executor/spawn.rs`
- Modify: `src/daemon_v2/executor/mod.rs`
- Modify: `src/daemon_v2/daemon.rs`

This is the critical task — connecting the command pipeline to real process spawning.

- [ ] **Step 1: Add spawn_agent() and stop_agent() to executor/spawn.rs**

Read `src/headless.rs` and `src/launch.rs` to understand the spawn API, then add:

```rust
use crate::headless::HeadlessSession;
use crate::paths::ProjectPaths;

/// Spawn a real Claude Code / Codex session.
/// Returns the HeadlessSession handle and the events to emit.
pub async fn spawn_agent(
    spawn_config: &SpawnConfig,
    paths: &ProjectPaths,
) -> Result<(HeadlessSession, Vec<DomainEvent>), String> {
    let launch_config = build_launch_config(spawn_config, paths.dir_key());
    let headless_config = launch_config.to_headless_config(paths);

    let session = HeadlessSession::spawn(&headless_config)
        .await
        .map_err(|e| format!("failed to spawn {}: {e}", spawn_config.name))?;

    let pid = session.pid().unwrap_or(0);
    let id = uuid::Uuid::new_v4().to_string();

    let events = agent_spawned_events(id, spawn_config, pid);
    Ok((session, events))
}

/// Stop a running session by killing the process.
pub async fn stop_agent(session: &mut HeadlessSession) -> Result<(), String> {
    session
        .kill()
        .await
        .map_err(|e| format!("failed to kill session: {e}"))
}
```

- [ ] **Step 2: Add session tracking to DaemonV2**

In `src/daemon_v2/daemon.rs`, add a sessions map and wire spawn/stop:

```rust
use std::collections::HashMap;
use crate::headless::HeadlessSession;
use crate::paths::ProjectPaths;

pub struct DaemonV2 {
    config: DaemonV2Config,
    paths: ProjectPaths,
    store: EventStore,
    projections: Projections,
    scheduler: Scheduler,
    sessions: HashMap<String, HeadlessSession>,  // agent_id → session handle
}
```

Update `DaemonV2::new()` to create `ProjectPaths::new(&config.dir_key)` and store it. Initialize `sessions: HashMap::new()`.

- [ ] **Step 3: Wire execute() to use sessions map**

Update `src/daemon_v2/executor/mod.rs` to take a mutable sessions map and paths:

```rust
pub async fn execute(
    command: Command,
    sessions: &mut HashMap<String, HeadlessSession>,
    paths: &ProjectPaths,
) -> Vec<DomainEvent> {
    match command {
        Command::SpawnAgent(config) => {
            match spawn::spawn_agent(&config, paths).await {
                Ok((session, events)) => {
                    // Extract agent_id from the AgentCreated event
                    if let Some(DomainEvent::AgentCreated { ref id, .. }) = events.first() {
                        sessions.insert(id.clone(), session);
                    }
                    events
                }
                Err(e) => {
                    tracing::error!(%e, "failed to spawn agent");
                    vec![]
                }
            }
        }
        Command::StopAgent { id, reason } => {
            if let Some(mut session) = sessions.remove(&id) {
                if let Err(e) = spawn::stop_agent(&mut session).await {
                    tracing::error!(%e, %id, "failed to stop agent");
                }
            }
            vec![DomainEvent::AgentStopped { id, reason }]
        }
        Command::PollProcessHealth => {
            let mut events = Vec::new();
            let mut dead_ids = Vec::new();

            for (id, session) in sessions.iter() {
                match session.try_wait() {
                    Ok(Some(_status)) => {
                        dead_ids.push(id.clone());
                    }
                    Ok(None) => {} // still running
                    Err(e) => {
                        tracing::warn!(%id, %e, "error checking process health");
                    }
                }
            }

            for id in dead_ids {
                sessions.remove(&id);
                events.push(DomainEvent::AgentStopped {
                    id,
                    reason: "process exited".into(),
                });
            }

            events
        }
        Command::ResetTask { task_id } => {
            vec![DomainEvent::TaskReset { task_id, reason: "agent died".into() }]
        }
        _ => {
            tracing::debug!(?command, "unhandled command");
            vec![]
        }
    }
}
```

- [ ] **Step 4: Update DaemonV2::run_due_decisions() to pass sessions**

Update the method to pass `&mut self.sessions` and `&self.paths` to `execute()`.

- [ ] **Step 5: Register dispatch and health-poll decisions in scheduler**

In `DaemonV2::new()`, add:

```rust
scheduler.register(
    "dispatch_pending_tasks",
    Duration::from_secs(5),
    |proj, _channel| decisions::dispatch::dispatch_pending_tasks(proj, 3),
);
scheduler.register(
    "stop_completed_agents",
    Duration::from_secs(5),
    |proj, _channel| decisions::dispatch::stop_completed_agents(proj),
);
scheduler.register(
    "poll_process_health",
    Duration::from_secs(10),
    |_proj, _channel| vec![Command::PollProcessHealth],
);
```

- [ ] **Step 6: Verify compilation**

Run: `cargo build 2>&1 | tail -5`

- [ ] **Step 7: Run all unit tests**

Run: `cargo test --lib daemon_v2 2>&1 | tail -10`
Expected: All tests pass (existing + new dispatch tests).

- [ ] **Step 8: Commit**

```bash
git add src/daemon_v2/
git commit -m "feat(daemon-v2): wire executor to real HeadlessSession spawning and health polling"
```

---

### Task 3: RPC task.create endpoint

**Files:**
- Modify: `src/daemon_v2/rpc/handlers.rs`
- Modify: `src/daemon_v2/rpc/mod.rs`
- Modify: `src/daemon_v2/rpc/rpc_tests.rs`
- Modify: `src/daemon_v2/daemon.rs`

We need a way to create tasks via RPC so E2E tests can trigger the full pipeline.

- [ ] **Step 1: Add task.create handler**

In `src/daemon_v2/rpc/handlers.rs`, add:

```rust
use crate::daemon_v2::events::DomainEvent;

pub fn handle_task_create(params: Option<&Value>) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let id = params.get("id").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing id"))?;
    let subject = params.get("subject").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing subject"))?;
    let channel = params.get("channel").and_then(|v| v.as_str())
        .unwrap_or("main");

    Ok(vec![DomainEvent::TaskCreated {
        id: id.to_string(),
        subject: subject.to_string(),
        channel: channel.to_string(),
        blocked_by: vec![],
    }])
}
```

- [ ] **Step 2: Wire task.create into dispatch**

In `src/daemon_v2/rpc/mod.rs`, update `dispatch_request()`:

```rust
"task.create" => {
    match handlers::handle_task_create(params) {
        Ok(events) => {
            // Return events for the daemon to apply
            return json!({
                "jsonrpc": "2.0",
                "result": { "ok": true, "events": events },
                "id": id,
            });
        }
        Err(err) => return err.to_json(&id),
    }
}
```

- [ ] **Step 3: Handle events from RPC in daemon.rs**

Update `handle_rpc_connection()` in daemon.rs to detect `task.create` responses that contain events, and apply them to the store and projections. This requires the daemon to check the RPC response for an `events` field and process them.

Alternative approach: make `dispatch_request()` return `(Value, Vec<DomainEvent>)` so the daemon can apply events after sending the response.

- [ ] **Step 4: Add tests**

In `rpc_tests.rs`, add:

```rust
#[test]
fn task_create_returns_events() {
    let params = json!({"id": "t1", "subject": "Fix bug", "channel": "main"});
    let events = handlers::handle_task_create(Some(&params)).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], DomainEvent::TaskCreated { id, subject, .. } if id == "t1" && subject == "Fix bug"));
}
```

- [ ] **Step 5: Verify tests pass and commit**

```bash
git add src/daemon_v2/rpc/ src/daemon_v2/daemon.rs
git commit -m "feat(daemon-v2): add task.create RPC endpoint"
```

---

### Task 4: E2E test — create task, agent spawns, health detected

**Files:**
- Modify: `tests/daemon_v2_e2e.rs`

This is the critical E2E test that proves the full pipeline works with real Claude sessions.

- [ ] **Step 1: Add test for task creation via RPC**

```rust
#[test]
#[ignore]
fn test_daemon_v2_task_create_shows_in_status() {
    let mut harness = V2Harness::new();
    assert!(harness.start(), "daemon v2 failed to start");

    // Create a task via RPC
    let response = harness.rpc_call("task.create", Some(serde_json::json!({
        "id": "t1",
        "subject": "Say hello",
        "channel": "main",
    }))).expect("task.create failed");
    assert!(response["error"].is_null(), "task.create error: {response}");

    // Verify task appears in status
    std::thread::sleep(std::time::Duration::from_secs(1));
    let status = harness.rpc_call("status", None).expect("status failed");
    // Task should be pending (or in_progress if dispatch already ran)
    let pending = status["result"]["tasks"]["pending"].as_u64().unwrap_or(0);
    let in_progress = status["result"]["tasks"]["in_progress"].as_u64().unwrap_or(0);
    assert!(pending + in_progress >= 1, "task should exist: {status}");
}
```

- [ ] **Step 2: Add test for real agent spawning**

```rust
#[test]
#[ignore]
fn test_daemon_v2_spawns_agent_for_task() {
    let mut harness = V2Harness::new();
    assert!(harness.start(), "daemon v2 failed to start");

    // Create a task — the dispatcher should spawn an agent for it
    let response = harness.rpc_call("task.create", Some(serde_json::json!({
        "id": "t1",
        "subject": "Print 'hello from daemon v2' and exit",
        "channel": "main",
    }))).expect("task.create failed");
    assert!(response["error"].is_null());

    // Wait for dispatcher to spawn an agent (5s dispatch interval + spawn time)
    // Poll status until we see a running agent or timeout
    let mut saw_agent = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if let Some(status) = harness.rpc_call("status", None) {
            let running = status["result"]["agents"]["running"].as_u64().unwrap_or(0);
            let total = status["result"]["agents"]["total"].as_u64().unwrap_or(0);
            if total > 0 {
                saw_agent = true;
                eprintln!("Agent spawned: total={total}, running={running}");
                break;
            }
        }
    }
    assert!(saw_agent, "expected agent to be spawned for task within 30s");
}
```

- [ ] **Step 3: Run E2E tests**

Run: `cargo build && cargo test --test daemon_v2_e2e -- --ignored --test-threads=1 2>&1 | tail -20`
Expected: All tests pass (4 existing + 2 new).

- [ ] **Step 4: Commit**

```bash
git add tests/daemon_v2_e2e.rs
git commit -m "feat(daemon-v2): add E2E tests for task creation and real agent spawning"
```

---

## Summary

After completing all 4 tasks, Phase 3 provides:

- **Dispatch decisions** — `dispatch_pending_tasks` (spawn agents for unblocked tasks), `stop_completed_agents` (stop workers whose tasks are done)
- **Real agent spawning** — `SpawnAgent` command wired to `HeadlessSession::spawn()` via `LaunchConfig → HeadlessConfig`
- **Health polling** — `PollProcessHealth` uses `try_wait()` to detect dead processes, emits `AgentStopped` events
- **Session tracking** — `HashMap<AgentId, HeadlessSession>` in DaemonV2
- **task.create RPC** — create tasks via JSON-RPC, verified via E2E
- **6 E2E tests** — including real Claude session spawning with OAuth tokens
