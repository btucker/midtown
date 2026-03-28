# Daemon v2 Phase 2: Agent Lifecycle and E2E Testing

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the v2 daemon's event-sourced core to real Claude Code/Codex processes. Spawn agents, monitor health, detect exits, resume on restart. Prove it works with real Claude sessions via E2E tests using existing OAuth tokens.

**Architecture:** The executor layer translates Commands into I/O (process spawning via existing `HeadlessSession`, health checks via `try_wait()`). Successful I/O emits DomainEvents that feed back into projections. A `DaemonV2` struct owns the event loop (scheduler + RPC listener). E2E tests use the existing `DaemonTestHarness`.

**Tech Stack:** Rust, tokio, existing `LaunchConfig`/`HeadlessSession` from `src/launch.rs`/`src/headless.rs`

**Spec:** `docs/superpowers/specs/2026-03-28-daemon-v2-design.md`

**Depends on:** Phase 1 (event store, projections, RPC skeleton)

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src/daemon_v2/executor/mod.rs` | Create | Command executor — dispatches commands to I/O handlers |
| `src/daemon_v2/executor/spawn.rs` | Create | Agent process spawning/stopping via HeadlessSession |
| `src/daemon_v2/executor/spawn_tests.rs` | Create | Unit tests for spawn config building |
| `src/daemon_v2/decisions/mod.rs` | Create | Command enum (move from rpc, expand) |
| `src/daemon_v2/decisions/health.rs` | Create | Process health and idle worker decisions |
| `src/daemon_v2/decisions/health_tests.rs` | Create | Tests for health decisions |
| `src/daemon_v2/scheduler.rs` | Create | Timer wheel for scheduled decisions |
| `src/daemon_v2/scheduler_tests.rs` | Create | Tests for scheduler |
| `src/daemon_v2/daemon.rs` | Create | DaemonV2 struct — owns event loop, projections, store |
| `src/daemon_v2/mod.rs` | Modify | Add new modules, run() entry point |
| `tests/daemon_v2_e2e.rs` | Create | E2E tests with real Claude sessions |

---

### Task 1: Command enum and decisions module

**Files:**
- Create: `src/daemon_v2/decisions/mod.rs`
- Create: `src/daemon_v2/decisions/health.rs`
- Create: `src/daemon_v2/decisions/health_tests.rs`
- Modify: `src/daemon_v2/mod.rs`

- [ ] **Step 1: Create src/daemon_v2/decisions/mod.rs with Command enum**

```rust
pub mod health;

use crate::daemon_v2::events::{AgentId, AgentKind, Provider, TaskId};

#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub name: String,
    pub kind: AgentKind,
    pub agent_type: String,
    pub provider: Provider,
    pub channel: Option<String>,
    pub task_id: Option<TaskId>,
    pub initial_prompt: Option<String>,
    pub working_dir: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Command {
    // Agent lifecycle
    SpawnAgent(SpawnConfig),
    StopAgent { id: AgentId, reason: String },
    ResumeAgent { id: AgentId },
    NudgeAgent { id: AgentId, message: String },

    // Work management
    AssignTask { task_id: TaskId, agent_id: AgentId },
    CompleteTask { task_id: TaskId },
    ResetTask { task_id: TaskId },

    // Communication
    Post { channel: String, sender: String, content: String, thread_id: Option<String> },
    PostSystem { channel: String, content: String },

    // Polling (executor performs I/O, emits events)
    PollProcessHealth,
}
```

- [ ] **Step 2: Create src/daemon_v2/decisions/health_tests.rs**

```rust
use super::*;
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::projections::cooldowns::{CooldownCategory, CooldownTracker};
use crate::daemon_v2::decisions::Command;

fn proj_with_running_agent(id: &str, name: &str, kind: AgentKind) -> Projections {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: id.into(), name: name.into(), kind,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
    });
    proj.apply(&DomainEvent::AgentStarted { id: id.into(), pid: 1234 });
    proj
}

#[test]
fn respawn_dead_agent_with_task() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "a1".into(), name: "worker-1".into(), kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(), provider: Provider::ClaudeCode,
        channel: Some("main".into()), task_id: Some("t1".into()),
    });
    proj.apply(&DomainEvent::AgentStarted { id: "a1".into(), pid: 1234 });
    // Simulate death detected
    proj.apply(&DomainEvent::AgentStopped { id: "a1".into(), reason: "process exited".into() });

    let commands = health::check_dead_workers(&proj);
    // Dead worker with a task should be reset
    assert!(commands.iter().any(|c| matches!(c, Command::ResetTask { task_id } if task_id == "t1")));
}

#[test]
fn no_respawn_for_completed_task() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(), subject: "Fix bug".into(), channel: "main".into(), blocked_by: vec![],
    });
    proj.apply(&DomainEvent::TaskAssigned { task_id: "t1".into(), agent_id: "a1".into() });
    proj.apply(&DomainEvent::TaskCompleted { task_id: "t1".into() });
    proj.apply(&DomainEvent::AgentCreated {
        id: "a1".into(), name: "worker-1".into(), kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(), provider: Provider::ClaudeCode,
        channel: Some("main".into()), task_id: Some("t1".into()),
    });
    proj.apply(&DomainEvent::AgentStopped { id: "a1".into(), reason: "completed".into() });

    let commands = health::check_dead_workers(&proj);
    // Completed task should NOT be reset
    assert!(commands.is_empty());
}

#[test]
fn ensure_leads_alive_spawns_missing_lead() {
    let proj = Projections::default();
    // No leads exist — should suggest spawning one
    let commands = health::ensure_leads_alive(&proj, "main");
    assert!(commands.iter().any(|c| matches!(c, Command::SpawnAgent(config) if config.kind == AgentKind::Lead)));
}

#[test]
fn ensure_leads_alive_no_op_when_running() {
    let proj = proj_with_running_agent("a1", "main-lead", AgentKind::Lead);
    let commands = health::ensure_leads_alive(&proj, "main");
    assert!(commands.is_empty());
}
```

- [ ] **Step 3: Create src/daemon_v2/decisions/health.rs**

```rust
#[path = "health_tests.rs"]
#[cfg(test)]
mod tests;

use crate::daemon_v2::decisions::{Command, SpawnConfig};
use crate::daemon_v2::events::{AgentKind, Provider, TaskStatus};
use crate::daemon_v2::projections::Projections;

/// Check for workers that stopped while their task is still in-progress.
/// Reset those tasks to pending so they can be re-dispatched.
pub fn check_dead_workers(proj: &Projections) -> Vec<Command> {
    let mut commands = Vec::new();

    for (id, agent) in &proj.agents.by_id {
        // Only care about stopped workers
        if proj.agents.running.contains(id) || agent.kind != AgentKind::Worker {
            continue;
        }
        // If agent has a task that's still in-progress, reset it
        if let Some(ref task_id) = agent.task_id {
            if let Some(task) = proj.work.tasks.get(task_id) {
                if task.status == TaskStatus::InProgress {
                    commands.push(Command::ResetTask {
                        task_id: task_id.clone(),
                    });
                }
            }
        }
    }

    commands
}

/// Ensure a lead agent exists and is running for the given default channel.
pub fn ensure_leads_alive(proj: &Projections, default_channel: &str) -> Vec<Command> {
    // Check if any running lead exists for this channel
    let has_running_lead = proj.agents.by_id.values().any(|a| {
        a.kind == AgentKind::Lead
            && proj.agents.running.contains(&a.id)
            && a.channel.as_deref() == Some(default_channel)
    });

    if has_running_lead {
        return vec![];
    }

    vec![Command::SpawnAgent(SpawnConfig {
        name: format!("{default_channel}-lead"),
        kind: AgentKind::Lead,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some(default_channel.to_string()),
        task_id: None,
        initial_prompt: None,
        working_dir: None,
        model: None,
    })]
}
```

- [ ] **Step 4: Update src/daemon_v2/mod.rs**

```rust
pub mod decisions;
pub mod events;
pub mod projections;
pub mod rpc;

pub use events::DomainEvent;
pub use projections::Projections;

#[path = "integration_tests.rs"]
#[cfg(test)]
mod integration_tests;
```

- [ ] **Step 5: Verify tests pass**

Run: `cargo test --lib daemon_v2::decisions 2>&1 | tail -10`
Expected: All 4 health tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/daemon_v2/decisions/
git commit -m "feat(daemon-v2): add Command enum and health decision functions"
```

---

### Task 2: Executor — spawn and stop agents

**Files:**
- Create: `src/daemon_v2/executor/mod.rs`
- Create: `src/daemon_v2/executor/spawn.rs`
- Create: `src/daemon_v2/executor/spawn_tests.rs`
- Modify: `src/daemon_v2/mod.rs`

- [ ] **Step 1: Create src/daemon_v2/executor/spawn_tests.rs**

These test that `SpawnConfig` correctly translates to `LaunchConfig` fields. No actual process spawning — that's E2E.

```rust
use super::*;
use crate::daemon_v2::decisions::SpawnConfig;
use crate::daemon_v2::events::{AgentKind, Provider};

#[test]
fn worker_spawn_config_to_launch_config() {
    let spawn = SpawnConfig {
        name: "ghost-town".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("task-42".into()),
        initial_prompt: Some("Fix the auth bug".into()),
        working_dir: Some("/tmp/worktree".into()),
        model: Some("sonnet".into()),
    };

    let launch = build_launch_config(&spawn, "test-project");
    assert_eq!(launch.name, "ghost-town");
    assert_eq!(launch.agent_type, "midtown-code-author");
    assert_eq!(launch.task_id, Some("task-42".to_string()));
    assert_eq!(launch.initial_prompt, Some("Fix the auth bug".to_string()));
}

#[test]
fn lead_spawn_config_to_launch_config() {
    let spawn = SpawnConfig {
        name: "main-lead".into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        initial_prompt: None,
        working_dir: None,
        model: None,
    };

    let launch = build_launch_config(&spawn, "test-project");
    assert_eq!(launch.name, "main-lead");
    assert_eq!(launch.agent_type, "midtown-channel-lead");
    assert!(launch.task_id.is_none());
}
```

- [ ] **Step 2: Create src/daemon_v2/executor/spawn.rs**

This bridges between the v2 `SpawnConfig` and the existing `LaunchConfig`. Read `src/launch.rs` to understand `LaunchConfig`'s constructor and builder methods, then implement `build_launch_config()`.

```rust
#[path = "spawn_tests.rs"]
#[cfg(test)]
mod tests;

use crate::daemon_v2::decisions::SpawnConfig;
use crate::daemon_v2::events::{AgentId, AgentKind, DomainEvent, Provider};
use crate::launch::LaunchConfig;

/// Convert a v2 SpawnConfig into a v1 LaunchConfig for process spawning.
/// This is the bridge between the new decision layer and existing process management.
pub fn build_launch_config(spawn: &SpawnConfig, dir_key: &str) -> LaunchConfig {
    let mut config = LaunchConfig::new(
        &spawn.name,
        &spawn.agent_type,
        dir_key,
        spawn.initial_prompt.clone(),
        None, // system_prompt_extra
    );

    if let Some(ref task_id) = spawn.task_id {
        config = config.with_task_id(task_id.clone());
    }

    if let Some(ref model) = spawn.model {
        config = config.with_model(model.clone());
    }

    if let Some(ref channel) = spawn.channel {
        config = config.with_channel(channel.clone());
    }

    config
}

/// Events emitted when an agent is successfully spawned.
pub fn agent_spawned_events(
    id: AgentId,
    spawn: &SpawnConfig,
    pid: u32,
) -> Vec<DomainEvent> {
    vec![
        DomainEvent::AgentCreated {
            id: id.clone(),
            name: spawn.name.clone(),
            kind: spawn.kind.clone(),
            agent_type: spawn.agent_type.clone(),
            provider: spawn.provider.clone(),
            channel: spawn.channel.clone(),
            task_id: spawn.task_id.clone(),
        },
        DomainEvent::AgentStarted { id, pid },
    ]
}
```

- [ ] **Step 3: Create src/daemon_v2/executor/mod.rs**

```rust
pub mod spawn;

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::DomainEvent;

/// Execute a command, performing I/O, and return the resulting domain events.
/// This is the only place in the v2 daemon where side effects happen.
///
/// Note: This is a skeleton — full implementation comes as each command
/// type is wired up. For now, only logging is implemented.
pub async fn execute(command: Command, _dir_key: &str) -> Vec<DomainEvent> {
    match command {
        Command::SpawnAgent(config) => {
            tracing::info!(name = %config.name, kind = ?config.kind, "would spawn agent");
            // Real spawn implementation comes in Task 4 (DaemonV2 struct)
            vec![]
        }
        Command::StopAgent { id, reason } => {
            tracing::info!(%id, %reason, "would stop agent");
            vec![DomainEvent::AgentStopped { id, reason }]
        }
        Command::ResetTask { task_id } => {
            tracing::info!(%task_id, "resetting task to pending");
            vec![DomainEvent::TaskReset {
                task_id,
                reason: "agent died".into(),
            }]
        }
        _ => {
            tracing::debug!(?command, "unhandled command");
            vec![]
        }
    }
}
```

- [ ] **Step 4: Update src/daemon_v2/mod.rs**

```rust
pub mod decisions;
pub mod events;
pub mod executor;
pub mod projections;
pub mod rpc;

pub use events::DomainEvent;
pub use projections::Projections;

#[path = "integration_tests.rs"]
#[cfg(test)]
mod integration_tests;
```

- [ ] **Step 5: Verify tests pass**

Run: `cargo test --lib daemon_v2 2>&1 | tail -15`
Expected: All tests pass (37 existing + 2 spawn + 4 health = 43).

- [ ] **Step 6: Commit**

```bash
git add src/daemon_v2/executor/ src/daemon_v2/mod.rs
git commit -m "feat(daemon-v2): add executor with spawn config bridge to LaunchConfig"
```

---

### Task 3: Scheduler — timer wheel for decisions

**Files:**
- Create: `src/daemon_v2/scheduler.rs`
- Create: `src/daemon_v2/scheduler_tests.rs`
- Modify: `src/daemon_v2/mod.rs`

- [ ] **Step 1: Create src/daemon_v2/scheduler_tests.rs**

```rust
use super::*;
use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::projections::Projections;
use std::time::Duration;

fn dummy_decision(_proj: &Projections, _default_channel: &str) -> Vec<Command> {
    vec![Command::PollProcessHealth]
}

#[test]
fn scheduler_returns_decisions_in_interval_order() {
    let mut scheduler = Scheduler::new();
    scheduler.register("fast", Duration::from_millis(10), dummy_decision);
    scheduler.register("slow", Duration::from_millis(100), dummy_decision);

    // Initially, all decisions are due
    let due = scheduler.due_decisions();
    assert_eq!(due.len(), 2);
}

#[test]
fn scheduler_respects_intervals() {
    let mut scheduler = Scheduler::new();
    scheduler.register("fast", Duration::from_millis(10), dummy_decision);

    // First call: due
    let due = scheduler.due_decisions();
    assert_eq!(due.len(), 1);
    scheduler.mark_ran("fast");

    // Immediately after: not due
    let due = scheduler.due_decisions();
    assert_eq!(due.len(), 0);
}

#[test]
fn next_deadline_returns_soonest() {
    let mut scheduler = Scheduler::new();
    scheduler.register("fast", Duration::from_millis(10), dummy_decision);
    scheduler.register("slow", Duration::from_secs(60), dummy_decision);
    scheduler.mark_ran("fast");
    scheduler.mark_ran("slow");

    let next = scheduler.next_deadline();
    assert!(next.is_some());
    // Next deadline should be ~10ms (the fast one), not 60s
    assert!(next.unwrap() < Duration::from_secs(1));
}
```

- [ ] **Step 2: Create src/daemon_v2/scheduler.rs**

```rust
#[path = "scheduler_tests.rs"]
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::projections::Projections;

pub type DecisionFn = fn(&Projections, &str) -> Vec<Command>;

struct ScheduledEntry {
    name: &'static str,
    interval: Duration,
    last_ran: Option<Instant>,
    run: DecisionFn,
}

pub struct Scheduler {
    entries: Vec<ScheduledEntry>,
}

pub struct DueDecision {
    pub name: &'static str,
    pub run: DecisionFn,
}

impl Scheduler {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn register(&mut self, name: &'static str, interval: Duration, run: DecisionFn) {
        self.entries.push(ScheduledEntry {
            name,
            interval,
            last_ran: None,
            run,
        });
    }

    /// Return all decisions whose interval has elapsed since they last ran.
    /// Decisions that have never run are always due.
    pub fn due_decisions(&self) -> Vec<DueDecision> {
        let now = Instant::now();
        self.entries
            .iter()
            .filter(|e| match e.last_ran {
                None => true,
                Some(last) => now.duration_since(last) >= e.interval,
            })
            .map(|e| DueDecision {
                name: e.name,
                run: e.run,
            })
            .collect()
    }

    /// Mark a decision as just having run.
    pub fn mark_ran(&mut self, name: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.name == name) {
            entry.last_ran = Some(Instant::now());
        }
    }

    /// How long until the next decision is due. Returns None if no decisions registered.
    pub fn next_deadline(&self) -> Option<Duration> {
        let now = Instant::now();
        self.entries
            .iter()
            .map(|e| match e.last_ran {
                None => Duration::ZERO,
                Some(last) => {
                    let elapsed = now.duration_since(last);
                    e.interval.saturating_sub(elapsed)
                }
            })
            .min()
    }
}
```

- [ ] **Step 3: Update src/daemon_v2/mod.rs**

```rust
pub mod decisions;
pub mod events;
pub mod executor;
pub mod projections;
pub mod rpc;
pub mod scheduler;

pub use events::DomainEvent;
pub use projections::Projections;

#[path = "integration_tests.rs"]
#[cfg(test)]
mod integration_tests;
```

- [ ] **Step 4: Verify tests pass**

Run: `cargo test --lib daemon_v2 2>&1 | tail -15`
Expected: All tests pass (43 + 3 scheduler = 46).

- [ ] **Step 5: Commit**

```bash
git add src/daemon_v2/scheduler.rs src/daemon_v2/scheduler_tests.rs src/daemon_v2/mod.rs
git commit -m "feat(daemon-v2): add scheduler with timer-based decision dispatch"
```

---

### Task 4: DaemonV2 struct — the event loop

**Files:**
- Create: `src/daemon_v2/daemon.rs`
- Modify: `src/daemon_v2/mod.rs`

This is the main loop that ties everything together: scheduler fires decisions, decisions produce commands, executor runs commands, events update projections.

- [ ] **Step 1: Create src/daemon_v2/daemon.rs**

```rust
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::sync::Mutex;

use crate::daemon_v2::decisions::{self, Command};
use crate::daemon_v2::events::{DomainEvent, EventStore};
use crate::daemon_v2::executor;
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::rpc;
use crate::daemon_v2::scheduler::Scheduler;

pub struct DaemonV2Config {
    pub dir_key: String,
    pub socket_path: PathBuf,
    pub events_dir: PathBuf,
    pub default_channel: String,
}

pub struct DaemonV2 {
    config: DaemonV2Config,
    store: EventStore,
    projections: Projections,
    scheduler: Scheduler,
}

pub enum DaemonV2ExitStatus {
    Shutdown,
}

impl DaemonV2 {
    /// Initialize daemon: recover state from disk, set up scheduler.
    pub fn new(config: DaemonV2Config) -> std::io::Result<Self> {
        let (store, snapshot, replay_events) =
            EventStore::recover(config.events_dir.clone())?;

        let mut projections = snapshot.unwrap_or_default();
        projections.apply_all(&replay_events);

        let mut scheduler = Scheduler::new();

        // Register health decisions
        scheduler.register(
            "check_dead_workers",
            Duration::from_secs(30),
            |proj, _channel| decisions::health::check_dead_workers(proj),
        );
        scheduler.register(
            "ensure_leads_alive",
            Duration::from_secs(30),
            |proj, channel| decisions::health::ensure_leads_alive(proj, channel),
        );

        tracing::info!(
            sequence = store.sequence(),
            agents = projections.agents.by_id.len(),
            tasks = projections.work.tasks.len(),
            "daemon v2 initialized"
        );

        Ok(Self {
            config,
            store,
            projections,
            scheduler,
        })
    }

    /// Run the main event loop.
    pub async fn run(mut self) -> DaemonV2ExitStatus {
        // Bind socket
        if self.config.socket_path.exists() {
            let _ = std::fs::remove_file(&self.config.socket_path);
        }
        let listener = match UnixListener::bind(&self.config.socket_path) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(%e, "failed to bind socket");
                return DaemonV2ExitStatus::Shutdown;
            }
        };
        tracing::info!(path = %self.config.socket_path.display(), "listening on socket");

        let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);

        loop {
            let next_deadline = self.scheduler.next_deadline().unwrap_or(Duration::from_secs(5));

            tokio::select! {
                // Accept RPC connections
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            self.handle_rpc_connection(stream).await;
                        }
                        Err(e) => {
                            tracing::error!(%e, "socket accept error");
                        }
                    }
                }

                // Scheduler tick
                _ = tokio::time::sleep(next_deadline) => {
                    self.run_due_decisions().await;
                }
            }
        }
    }

    async fn handle_rpc_connection(&mut self, stream: tokio::net::UnixStream) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        if reader.read_line(&mut line).await.is_ok() && !line.is_empty() {
            if let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) {
                // Check for shutdown
                if request.get("method").and_then(|m| m.as_str()) == Some("shutdown") {
                    let id = request.get("id").cloned().unwrap_or(serde_json::Value::Null);
                    let response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "result": "shutting down",
                        "id": id,
                    });
                    let response_str = serde_json::to_string(&response).unwrap_or_default();
                    let _ = writer.write_all(response_str.as_bytes()).await;
                    let _ = writer.write_all(b"\n").await;
                    let _ = writer.flush().await;
                    // Exit the event loop
                    std::process::exit(0);
                }

                let response = rpc::dispatch_request(request, &self.projections);
                let response_str = serde_json::to_string(&response).unwrap_or_default();
                let _ = writer.write_all(response_str.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
                let _ = writer.flush().await;
            }
        }
    }

    async fn run_due_decisions(&mut self) {
        let due = self.scheduler.due_decisions();
        for decision in &due {
            let commands = (decision.run)(&self.projections, &self.config.default_channel);
            self.scheduler.mark_ran(decision.name);

            for command in commands {
                let events = executor::execute(command, &self.config.dir_key).await;
                for event in &events {
                    if let Err(e) = self.store.append(event) {
                        tracing::error!(%e, "failed to append event");
                    }
                    self.projections.apply(event);
                }
            }
        }
    }
}
```

- [ ] **Step 2: Update src/daemon_v2/mod.rs with run() entry point**

```rust
pub mod daemon;
pub mod decisions;
pub mod events;
pub mod executor;
pub mod projections;
pub mod rpc;
pub mod scheduler;

pub use daemon::{DaemonV2, DaemonV2Config, DaemonV2ExitStatus};
pub use events::DomainEvent;
pub use projections::Projections;

#[path = "integration_tests.rs"]
#[cfg(test)]
mod integration_tests;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1 | tail -10`
Expected: Compiles. No tests to run for this task — it's all async I/O that needs E2E.

- [ ] **Step 4: Commit**

```bash
git add src/daemon_v2/daemon.rs src/daemon_v2/mod.rs
git commit -m "feat(daemon-v2): add DaemonV2 event loop with scheduler and RPC"
```

---

### Task 5: CLI entry point for daemon-v2

**Files:**
- Modify: `src/bin/midtown/main.rs`

Add a `daemon-v2` subcommand that starts the v2 daemon. This enables E2E testing.

- [ ] **Step 1: Read src/bin/midtown/main.rs to find the Commands enum**

Read the file and find where `Commands::Daemon` is defined and where it's handled.

- [ ] **Step 2: Add DaemonV2 variant to Commands enum**

Add alongside the existing `Daemon` variant:

```rust
/// Start daemon v2 (event-sourced) — experimental
#[clap(hide = true)]
DaemonV2 {
    /// Path to the Unix socket
    #[clap(long)]
    socket: PathBuf,

    /// Working directory (git repo)
    #[clap(long)]
    workdir: PathBuf,

    /// Default channel name
    #[clap(long, default_value = "main")]
    channel: String,
},
```

- [ ] **Step 3: Handle the DaemonV2 command**

In the match on `command`, add:

```rust
Commands::DaemonV2 { socket, workdir, channel } => {
    let dir_key = workdir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let events_dir = midtown::paths::project_dir(&dir_key).join("events");
    let config = midtown::daemon_v2::DaemonV2Config {
        dir_key,
        socket_path: socket,
        events_dir,
        default_channel: channel,
    };

    let daemon = midtown::daemon_v2::DaemonV2::new(config)
        .expect("failed to initialize daemon v2");

    let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");
    rt.block_on(daemon.run());
}
```

- [ ] **Step 4: Verify compilation**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles successfully.

- [ ] **Step 5: Commit**

```bash
git add src/bin/midtown/main.rs
git commit -m "feat(daemon-v2): add daemon-v2 CLI subcommand"
```

---

### Task 6: E2E test — spawn daemon, RPC status, shutdown

**Files:**
- Create: `tests/daemon_v2_e2e.rs`

This test starts a real v2 daemon process, connects via socket, queries status, and shuts down. Uses real Claude auth tokens via existing OAuth setup.

- [ ] **Step 1: Create tests/daemon_v2_e2e.rs**

```rust
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

/// Test harness for daemon v2 E2E tests
struct V2Harness {
    socket_path: PathBuf,
    _temp_dir: tempfile::TempDir,
    state_dir: tempfile::TempDir,
    daemon: Option<Child>,
}

impl V2Harness {
    fn new() -> Self {
        let temp_dir = tempfile::TempDir::new().expect("create temp dir");
        let state_dir = tempfile::TempDir::new().expect("create state dir");

        // Initialize a git repo in the temp dir
        let status = Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git init");
        assert!(status.status.success());

        // Initial commit so daemon can detect the repo
        let status = Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git commit");
        assert!(status.status.success());

        let socket_path = state_dir.path().join("daemon-v2.sock");

        Self {
            socket_path,
            _temp_dir: temp_dir,
            state_dir,
            daemon: None,
        }
    }

    fn start(&mut self) -> bool {
        let child = Command::new(env!("CARGO_BIN_EXE_midtown"))
            .args([
                "daemon-v2",
                "--socket", self.socket_path.to_str().unwrap(),
                "--workdir", self._temp_dir.path().to_str().unwrap(),
                "--channel", "main",
            ])
            .env("XDG_STATE_HOME", self.state_dir.path())
            .env("MIDTOWN_CHAT_MONITOR", "0")
            .env("MIDTOWN_WEBHOOK_PORT", "0")
            .spawn()
            .expect("spawn daemon v2");

        self.daemon = Some(child);

        // Wait for socket to appear
        for _ in 0..100 {
            if self.socket_path.exists() {
                // Try connecting
                if UnixStream::connect(&self.socket_path).is_ok() {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    fn rpc_call(&self, method: &str, params: Option<serde_json::Value>) -> Option<serde_json::Value> {
        let mut stream = UnixStream::connect(&self.socket_path).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1,
        });
        let request_str = serde_json::to_string(&request).ok()?;
        stream.write_all(request_str.as_bytes()).ok()?;
        stream.write_all(b"\n").ok()?;
        stream.flush().ok()?;

        let mut reader = BufReader::new(&stream);
        let mut response = String::new();
        reader.read_line(&mut response).ok()?;

        serde_json::from_str(&response).ok()
    }

    fn stop(&mut self) {
        // Try graceful shutdown via RPC
        let _ = self.rpc_call("shutdown", None);
        std::thread::sleep(Duration::from_millis(500));

        // Force kill if still running
        if let Some(ref mut child) = self.daemon {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for V2Harness {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
#[ignore] // Requires built binary
fn test_daemon_v2_starts_and_responds_to_status() {
    let mut harness = V2Harness::new();
    assert!(harness.start(), "daemon v2 failed to start");

    // Query status
    let response = harness.rpc_call("status", None).expect("status call failed");
    assert!(response["error"].is_null(), "status returned error: {response}");
    assert_eq!(response["result"]["agents"]["total"], 0);
    assert_eq!(response["result"]["tasks"]["pending"], 0);
}

#[test]
#[ignore] // Requires built binary
fn test_daemon_v2_agent_list_empty() {
    let mut harness = V2Harness::new();
    assert!(harness.start(), "daemon v2 failed to start");

    let response = harness.rpc_call("agent.list", None).expect("agent.list failed");
    assert!(response["error"].is_null());
    let agents = response["result"].as_array().expect("result should be array");
    assert!(agents.is_empty());
}

#[test]
#[ignore] // Requires built binary
fn test_daemon_v2_unknown_method_returns_error() {
    let mut harness = V2Harness::new();
    assert!(harness.start(), "daemon v2 failed to start");

    let response = harness.rpc_call("nonexistent.method", None).expect("rpc call failed");
    assert_eq!(response["error"]["code"], -32601);
}

#[test]
#[ignore] // Requires built binary
fn test_daemon_v2_shutdown() {
    let mut harness = V2Harness::new();
    assert!(harness.start(), "daemon v2 failed to start");

    // Verify it's responding
    let response = harness.rpc_call("status", None);
    assert!(response.is_some());

    // Shutdown
    harness.stop();

    // Socket should be gone or connection should fail
    std::thread::sleep(Duration::from_millis(500));
    let result = UnixStream::connect(&harness.socket_path);
    assert!(result.is_err(), "daemon should be stopped");
}
```

- [ ] **Step 2: Build and run E2E tests**

Run: `cargo build && cargo test --test daemon_v2_e2e -- --ignored --test-threads=1 2>&1 | tail -20`
Expected: All 4 E2E tests pass.

- [ ] **Step 3: Commit**

```bash
git add tests/daemon_v2_e2e.rs
git commit -m "feat(daemon-v2): add E2E tests for daemon startup, RPC, and shutdown"
```

---

## Summary

After completing all 6 tasks, Phase 2 provides:

- **Command enum** — agent lifecycle + work management + polling commands
- **Health decisions** — `check_dead_workers` (reset orphaned tasks), `ensure_leads_alive` (spawn missing lead)
- **Executor skeleton** — command dispatch with spawn config bridge to existing `LaunchConfig`
- **Scheduler** — timer wheel firing decision functions at configurable intervals
- **DaemonV2 struct** — full event loop: socket listener + scheduler + RPC dispatch
- **CLI entry point** — `midtown daemon-v2` subcommand
- **4 E2E tests** — real daemon process, real socket, real RPC calls
- **~46 unit tests + 4 E2E tests**

The v2 daemon can start, respond to RPC queries, and shut down. It doesn't yet spawn real Claude sessions — that comes in Phase 3 when we wire `SpawnAgent` to `HeadlessSession::spawn()`.
