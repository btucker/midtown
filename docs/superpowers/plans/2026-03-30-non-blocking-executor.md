# Non-Blocking Executor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the daemon's event loop non-blocking by running slow commands (spawn, HTTP, CLI) in background tasks, keeping the loop responsive to user messages.

**Architecture:** Split `executor::execute()` into `dispatch_command()` which classifies commands as inline (fast, returns events) or background (slow, spawns task that sends results via mpsc channel). Main loop gains a new `result_rx` arm in `select!` to receive background results and insert sessions. A `pending_lifecycle` map stashes nudges during in-flight spawn/stop operations.

**Tech Stack:** Rust, tokio (spawn, mpsc, select!)

---

### File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `src/daemon_v2/executor/mod.rs` | Modify | Add `ExecutorResult` enum, `dispatch_command()`, refactor background spawn/stop/poll |
| `src/daemon_v2/executor/dispatch_tests.rs` | Create | Tests for command classification and lifecycle guard |
| `src/daemon_v2/daemon.rs` | Modify | Add `result_rx` arm, `pending_lifecycle` map, replace `execute()` calls with `dispatch_command()` |

---

### Task 1: Define `ExecutorResult` and `dispatch_command` signature

**Files:**
- Modify: `src/daemon_v2/executor/mod.rs`
- Create: `src/daemon_v2/executor/dispatch_tests.rs`

- [ ] **Step 1: Write failing test for dispatch classification**

Create `src/daemon_v2/executor/dispatch_tests.rs`:

```rust
use super::*;
use crate::daemon_v2::decisions::Command;

#[test]
fn assign_task_is_inline() {
    let cmd = Command::AssignTask {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    };
    assert!(
        matches!(classify_command(&cmd), CommandClass::Inline),
        "AssignTask should be inline"
    );
}

#[test]
fn poll_prs_is_background() {
    let cmd = Command::PollPrs;
    assert!(
        matches!(classify_command(&cmd), CommandClass::Background),
        "PollPrs should be background"
    );
}

#[test]
fn spawn_agent_is_background() {
    let cmd = Command::SpawnAgent(SpawnConfig {
        name: "test".into(),
        kind: crate::daemon_v2::events::AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: crate::daemon_v2::events::Provider::ClaudeCode,
        channel: None,
        task_id: None,
        initial_prompt: None,
        working_dir: None,
        model: None,
        bound_thread_id: None,
        fork_from_session: None,
        icon: None,
        color: None,
    });
    assert!(
        matches!(classify_command(&cmd), CommandClass::Background),
        "SpawnAgent should be background"
    );
}

#[test]
fn nudge_deliver_is_inline() {
    // NudgeAgent is classified at dispatch time based on resolve_nudge_action,
    // not at classify time. The command itself is always "needs resolution".
    let cmd = Command::NudgeAgent {
        id: "a1".into(),
        message: "hello".into(),
    };
    assert!(
        matches!(classify_command(&cmd), CommandClass::NeedsResolution),
        "NudgeAgent needs runtime resolution"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib daemon_v2::executor::dispatch_tests`
Expected: FAIL — `classify_command` and `CommandClass` don't exist

- [ ] **Step 3: Add `ExecutorResult`, `CommandClass`, `classify_command`, and wire test module**

In `src/daemon_v2/executor/mod.rs`, add at the top (after existing imports):

```rust
#[path = "dispatch_tests.rs"]
#[cfg(test)]
mod dispatch_tests;

/// Result sent from background tasks back to the main event loop.
pub enum ExecutorResult {
    /// Events to apply to store + projections + broadcast.
    Events(Vec<DomainEvent>),
    /// A new session is ready — main loop inserts into sessions map.
    SessionReady {
        id: String,
        session: HeadlessSession,
        events: Vec<DomainEvent>,
    },
    /// A lifecycle operation (stop) completed — deliver stashed nudges.
    LifecycleComplete {
        id: String,
        events: Vec<DomainEvent>,
    },
}

/// Classification of how a command should be executed.
pub enum CommandClass {
    /// Execute immediately in the main loop (fast, may need &mut sessions).
    Inline,
    /// Execute in a background tokio task (slow I/O).
    Background,
    /// Needs runtime resolution (NudgeAgent — may be inline or background).
    NeedsResolution,
}

/// Classify a command as inline, background, or needs-resolution.
pub fn classify_command(cmd: &Command) -> CommandClass {
    match cmd {
        // Inline: pure event emission
        Command::AssignTask { .. }
        | Command::CompleteTask { .. }
        | Command::ResetTask { .. }
        | Command::GarbageCollect { .. }
        | Command::CreateWorktree { .. }
        | Command::RemoveWorktree { .. }
        | Command::Post { .. }
        | Command::PostSystem { .. } => CommandClass::Inline,

        // Inline: needs &mut sessions, but fast
        Command::PollProcessHealth => CommandClass::Inline,

        // Background: slow I/O
        Command::SpawnAgent(_)
        | Command::ResumeAgent { .. }
        | Command::StopAgent { .. }
        | Command::PollPrs
        | Command::MergePr { .. }
        | Command::PostPrComment { .. }
        | Command::RerunCi { .. } => CommandClass::Background,

        // Needs resolution at dispatch time
        Command::NudgeAgent { .. } => CommandClass::NeedsResolution,
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib daemon_v2::executor::dispatch_tests`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add src/daemon_v2/executor/mod.rs src/daemon_v2/executor/dispatch_tests.rs
git commit -m "feat(executor): add ExecutorResult, CommandClass, classify_command"
```

---

### Task 2: Extract inline execution into `execute_inline`

Move the fast, inline command handlers out of `execute()` into a new function that doesn't touch background commands.

**Files:**
- Modify: `src/daemon_v2/executor/mod.rs`

- [ ] **Step 1: Write failing test**

Add to `dispatch_tests.rs`:

```rust
#[tokio::test]
async fn execute_inline_handles_assign_task() {
    let mut sessions = HashMap::new();
    let cmd = Command::AssignTask {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    };
    let events = execute_inline(cmd, &mut sessions);
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], DomainEvent::TaskAssigned { task_id, agent_id }
        if task_id == "t1" && agent_id == "a1"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib daemon_v2::executor::dispatch_tests::execute_inline_handles_assign_task`
Expected: FAIL — `execute_inline` doesn't exist

- [ ] **Step 3: Create `execute_inline`**

Extract all `CommandClass::Inline` match arms from `execute()` into a new synchronous function:

```rust
/// Execute a command that is known to be fast and inline.
/// These commands either emit pure events or need &mut sessions but are non-blocking.
pub fn execute_inline(
    command: Command,
    sessions: &mut HashMap<String, HeadlessSession>,
    channels_dir: &Path,
) -> Vec<DomainEvent> {
    match command {
        Command::PollProcessHealth => {
            let mut events = vec![];
            let mut dead_ids = vec![];
            for (id, session) in sessions.iter_mut() {
                match session.try_wait() {
                    Ok(Some(_)) => {
                        dead_ids.push(id.clone());
                        events.push(DomainEvent::AgentStopped {
                            id: id.clone(),
                            reason: "process exited".into(),
                        });
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(%id, %e, "try_wait error"),
                }
            }
            for id in dead_ids {
                sessions.remove(&id);
            }
            events
        }
        Command::AssignTask { task_id, agent_id } => {
            vec![DomainEvent::TaskAssigned { task_id, agent_id }]
        }
        Command::CompleteTask { task_id } => {
            vec![DomainEvent::TaskCompleted { task_id }]
        }
        Command::ResetTask { task_id } => {
            tracing::info!(%task_id, "resetting task to pending");
            vec![DomainEvent::TaskReset { task_id, reason: "agent died".into() }]
        }
        Command::GarbageCollect { agent_id } => {
            tracing::info!(%agent_id, "garbage collecting agent record");
            vec![DomainEvent::AgentGarbageCollected { id: agent_id }]
        }
        Command::CreateWorktree { task_id, branch } => {
            tracing::debug!(%task_id, %branch, "CreateWorktree (managed by daemon)");
            vec![]
        }
        Command::RemoveWorktree { task_id } => {
            tracing::debug!(%task_id, "RemoveWorktree (managed by daemon)");
            vec![]
        }
        Command::Post { channel, sender, content, thread_id } => {
            if let Err(e) = channel_io::post_message(
                channels_dir, &channel, &sender, &content, thread_id.as_deref(),
            ) {
                tracing::error!(%e, %channel, "failed to post message");
                return vec![];
            }
            vec![DomainEvent::MessagePosted {
                id: uuid::Uuid::new_v4().to_string(),
                channel, sender, content, thread_id,
            }]
        }
        Command::PostSystem { channel, content } => {
            if let Err(e) = channel_io::post_system_message(channels_dir, &channel, &content) {
                tracing::error!(%e, %channel, "failed to post system message");
                return vec![];
            }
            vec![DomainEvent::MessagePosted {
                id: uuid::Uuid::new_v4().to_string(),
                channel, sender: "midtown".into(), content, thread_id: None,
            }]
        }
        other => {
            tracing::error!("execute_inline called with non-inline command: {other:?}");
            vec![]
        }
    }
}
```

Note: `Command` needs `Debug` derive if not already present. Check and add if needed.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib daemon_v2::executor::dispatch_tests`
Expected: 5 tests PASS

- [ ] **Step 5: Commit**

```bash
git add src/daemon_v2/executor/mod.rs src/daemon_v2/executor/dispatch_tests.rs
git commit -m "feat(executor): extract execute_inline for fast commands"
```

---

### Task 3: Create `spawn_background` for slow commands

Extract the slow command handlers into functions that run in `tokio::spawn` and send results via the `result_tx` channel.

**Files:**
- Modify: `src/daemon_v2/executor/mod.rs`
- Modify: `src/daemon_v2/executor/dispatch_tests.rs`

- [ ] **Step 1: Write failing test**

Add to `dispatch_tests.rs`:

```rust
#[tokio::test]
async fn spawn_background_poll_prs_sends_events() {
    let (result_tx, mut result_rx) = tokio::sync::mpsc::channel::<ExecutorResult>(16);
    let work = crate::daemon_v2::projections::work::WorkIndex::default();

    // PollPrs will fail (no gh CLI in test) but should send back result, not panic
    spawn_background_poll_prs(work, result_tx);

    // Should receive a result (empty events or error)
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        result_rx.recv(),
    ).await;
    assert!(result.is_ok(), "should receive result from background task");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib daemon_v2::executor::dispatch_tests::spawn_background_poll_prs`
Expected: FAIL — `spawn_background_poll_prs` doesn't exist

- [ ] **Step 3: Create background spawn functions**

Add to `src/daemon_v2/executor/mod.rs`:

```rust
/// Spawn PollPrs in a background task. Results sent via result_tx.
pub fn spawn_background_poll_prs(
    work: crate::daemon_v2::projections::work::WorkIndex,
    result_tx: tokio::sync::mpsc::Sender<ExecutorResult>,
) {
    tokio::spawn(async move {
        if let Some(status) = github::check_rate_limit().await
            && github::should_throttle(&status)
        {
            tracing::warn!(remaining = status.remaining, "PR polling skipped — rate limit low");
            return;
        }
        let events = match (
            github::fetch_open_prs().await,
            github::fetch_merged_prs().await,
        ) {
            (Ok(open), Ok(merged)) => github::diff_pr_state(&work, &open, &merged),
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!(%e, "PR polling failed");
                vec![]
            }
        };
        if !events.is_empty() {
            let _ = result_tx.send(ExecutorResult::Events(events)).await;
        }
    });
}

/// Spawn agent in a background task. Session + events sent via result_tx.
pub fn spawn_background_agent(
    config: SpawnConfig,
    paths: ProjectPaths,
    channels_dir: std::path::PathBuf,
    event_tx: tokio::sync::broadcast::Sender<DomainEvent>,
    result_tx: tokio::sync::mpsc::Sender<ExecutorResult>,
) {
    tokio::spawn(async move {
        match spawn::spawn_agent(&config, &paths).await {
            Ok((mut session, events)) => {
                drain_session_output(
                    &mut session,
                    &config.name,
                    config.channel.as_deref(),
                    &channels_dir,
                    &event_tx,
                );
                let id = events.iter().find_map(|e| match e {
                    DomainEvent::AgentCreated { id, .. } => Some(id.clone()),
                    _ => None,
                }).unwrap_or_default();

                // Auto-create DM channel
                if config.channel.is_none() && config.bound_thread_id.is_none() {
                    let dm = crate::daemon_v2::decisions::lifecycle::create_dm_channel_name(&config.name);
                    let _ = channel_io::post_system_message(&channels_dir, &dm, &format!("DM channel for {}", config.name));
                }

                let _ = result_tx.send(ExecutorResult::SessionReady { id, session, events }).await;
            }
            Err(e) => {
                tracing::error!(%e, name = %config.name, "failed to spawn agent");
                let _ = result_tx.send(ExecutorResult::Events(vec![
                    DomainEvent::AgentSpawnFailed {
                        name: config.name.clone(),
                        agent_type: config.agent_type.clone(),
                        reason: e.to_string(),
                    }
                ])).await;
            }
        }
    });
}

/// Resume agent in a background task.
pub fn spawn_background_resume(
    agent_id: String,
    agent: crate::daemon_v2::projections::agents::Agent,
    paths: ProjectPaths,
    result_tx: tokio::sync::mpsc::Sender<ExecutorResult>,
) {
    tokio::spawn(async move {
        let config = spawn_config_from_agent(&agent);
        let session_id = match &agent.session_id {
            Some(sid) => sid.clone(),
            None => {
                let _ = result_tx.send(ExecutorResult::Events(vec![
                    DomainEvent::AgentSpawnFailed {
                        name: agent.name.clone(),
                        agent_type: agent.agent_type.clone(),
                        reason: "no session_id for resume".into(),
                    }
                ])).await;
                return;
            }
        };
        let launch_config = spawn::build_launch_config(&config, paths.dir_key());
        let mut headless_config = launch_config.to_headless_config(&paths);
        headless_config.resume_session_id = Some(session_id);

        match HeadlessSession::spawn(&headless_config).await {
            Ok(session) => {
                let pid = session.pid().unwrap_or(0);
                let _ = result_tx.send(ExecutorResult::SessionReady {
                    id: agent_id.clone(),
                    session,
                    events: vec![DomainEvent::AgentResumed { id: agent_id, pid }],
                }).await;
            }
            Err(e) => {
                let _ = result_tx.send(ExecutorResult::Events(vec![
                    DomainEvent::AgentSpawnFailed {
                        name: agent.name.clone(),
                        agent_type: agent.agent_type.clone(),
                        reason: format!("resume failed: {e}"),
                    }
                ])).await;
            }
        }
    });
}

/// Background stop: kill the session process.
pub fn spawn_background_stop(
    id: String,
    reason: String,
    mut session: HeadlessSession,
    result_tx: tokio::sync::mpsc::Sender<ExecutorResult>,
) {
    tokio::spawn(async move {
        if let Err(e) = spawn::stop_agent(&mut session).await {
            tracing::warn!(%id, %e, "error stopping agent");
            let _ = result_tx.send(ExecutorResult::LifecycleComplete {
                id: id.clone(),
                events: vec![DomainEvent::AgentStopFailed { id, reason: e.to_string() }],
            }).await;
            return;
        }
        let _ = result_tx.send(ExecutorResult::LifecycleComplete {
            id: id.clone(),
            events: vec![DomainEvent::AgentStopped { id, reason }],
        }).await;
    });
}

/// Background gh CLI command (merge, comment, rerun).
pub fn spawn_background_gh_command(
    command: Command,
    result_tx: tokio::sync::mpsc::Sender<ExecutorResult>,
) {
    tokio::spawn(async move {
        let events = match command {
            Command::MergePr { number } => {
                tracing::info!(%number, "merging PR");
                match tokio::process::Command::new("gh")
                    .args(["pr", "merge", &number.to_string(), "--squash", "--auto"])
                    .output().await
                {
                    Ok(output) if output.status.success() => {
                        vec![DomainEvent::PrMerged { number, branch: String::new() }]
                    }
                    Ok(output) => {
                        let err = String::from_utf8_lossy(&output.stderr);
                        tracing::error!(%number, %err, "gh pr merge failed");
                        vec![]
                    }
                    Err(e) => { tracing::error!(%number, %e, "gh pr merge failed"); vec![] }
                }
            }
            Command::PostPrComment { number, body } => {
                tracing::info!(%number, "posting PR comment");
                match tokio::process::Command::new("gh")
                    .args(["pr", "comment", &number.to_string(), "--body", &body])
                    .output().await
                {
                    Ok(output) if !output.status.success() => {
                        let err = String::from_utf8_lossy(&output.stderr);
                        tracing::warn!(%number, %err, "gh pr comment failed");
                    }
                    Err(e) => tracing::warn!(%number, %e, "gh pr comment failed"),
                    _ => {}
                }
                vec![]
            }
            Command::RerunCi { run_id } => {
                tracing::info!(%run_id, "rerunning CI");
                match tokio::process::Command::new("gh")
                    .args(["run", "rerun", &run_id.to_string()])
                    .output().await
                {
                    Ok(output) if !output.status.success() => {
                        let err = String::from_utf8_lossy(&output.stderr);
                        tracing::warn!(%run_id, %err, "gh run rerun failed");
                    }
                    Err(e) => tracing::warn!(%run_id, %e, "gh run rerun failed"),
                    _ => {}
                }
                vec![]
            }
            _ => vec![],
        };
        if !events.is_empty() {
            let _ = result_tx.send(ExecutorResult::Events(events)).await;
        }
    });
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib daemon_v2::executor::dispatch_tests`
Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add src/daemon_v2/executor/mod.rs src/daemon_v2/executor/dispatch_tests.rs
git commit -m "feat(executor): add background spawn functions for slow commands"
```

---

### Task 4: Add lifecycle guard and NudgeAgent dispatch

**Files:**
- Modify: `src/daemon_v2/executor/mod.rs`
- Modify: `src/daemon_v2/executor/dispatch_tests.rs`

- [ ] **Step 1: Write failing test for lifecycle guard**

Add to `dispatch_tests.rs`:

```rust
#[test]
fn lifecycle_guard_stashes_nudge_during_spawn() {
    let mut guard = LifecycleGuard::new();
    guard.mark_spawning("agent-1".into());

    assert!(guard.is_pending("agent-1"));
    guard.stash_nudge("agent-1", "hello".into());

    let stashed = guard.complete("agent-1");
    assert_eq!(stashed, vec!["hello".to_string()]);
    assert!(!guard.is_pending("agent-1"));
}

#[test]
fn lifecycle_guard_returns_empty_when_no_stashed() {
    let mut guard = LifecycleGuard::new();
    guard.mark_spawning("agent-1".into());

    let stashed = guard.complete("agent-1");
    assert!(stashed.is_empty());
}

#[test]
fn lifecycle_guard_not_pending_for_unknown() {
    let guard = LifecycleGuard::new();
    assert!(!guard.is_pending("nonexistent"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib daemon_v2::executor::dispatch_tests::lifecycle_guard`
Expected: FAIL — `LifecycleGuard` doesn't exist

- [ ] **Step 3: Implement `LifecycleGuard`**

Add to `src/daemon_v2/executor/mod.rs`:

```rust
/// Tracks agents with in-flight lifecycle operations (spawn, stop).
/// Nudges arriving during these operations are stashed and delivered when complete.
pub struct LifecycleGuard {
    pending: HashMap<String, Vec<String>>,
}

impl LifecycleGuard {
    pub fn new() -> Self {
        Self { pending: HashMap::new() }
    }

    /// Mark an agent as having an in-flight spawn or stop.
    pub fn mark_spawning(&mut self, agent_id: String) {
        self.pending.entry(agent_id).or_default();
    }

    /// Check if an agent has an in-flight operation.
    pub fn is_pending(&self, agent_id: &str) -> bool {
        self.pending.contains_key(agent_id)
    }

    /// Check if any agent in a channel has an in-flight spawn (for dedup).
    pub fn has_pending_for_channel(&self, channel: &str, agents: &crate::daemon_v2::projections::agents::AgentIndex) -> bool {
        self.pending.keys().any(|id| {
            agents.by_id.get(id).is_some_and(|a| a.channel.as_deref() == Some(channel))
        })
    }

    /// Stash a nudge message for delivery after the operation completes.
    pub fn stash_nudge(&mut self, agent_id: &str, message: String) {
        if let Some(stashed) = self.pending.get_mut(agent_id) {
            stashed.push(message);
        }
    }

    /// Complete a lifecycle operation. Returns stashed nudge messages.
    pub fn complete(&mut self, agent_id: &str) -> Vec<String> {
        self.pending.remove(agent_id).unwrap_or_default()
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib daemon_v2::executor::dispatch_tests`
Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add src/daemon_v2/executor/mod.rs src/daemon_v2/executor/dispatch_tests.rs
git commit -m "feat(executor): add LifecycleGuard for stashing nudges during spawn/stop"
```

---

### Task 5: Wire dispatch into daemon event loop

Replace all `executor::execute()` calls in `daemon.rs` with the new dispatch pattern.

**Files:**
- Modify: `src/daemon_v2/daemon.rs`

- [ ] **Step 1: Add result channel and lifecycle guard to DaemonV2**

Add fields to the `DaemonV2` struct:

```rust
/// Receiver for background executor results.
result_rx: tokio::sync::mpsc::Receiver<executor::ExecutorResult>,
/// Sender cloned into background tasks.
result_tx: tokio::sync::mpsc::Sender<executor::ExecutorResult>,
/// Tracks in-flight spawn/stop operations; stashes nudges.
lifecycle_guard: executor::LifecycleGuard,
```

Initialize in `new()`:

```rust
let (result_tx, result_rx) = tokio::sync::mpsc::channel::<executor::ExecutorResult>(64);
```

- [ ] **Step 2: Create `dispatch_command` method on DaemonV2**

```rust
/// Dispatch a command: inline commands execute immediately, background commands
/// are spawned as tasks that send results via result_tx.
async fn dispatch_command(&mut self, command: Command) {
    use executor::{classify_command, CommandClass};

    match classify_command(&command) {
        CommandClass::Inline => {
            let events = executor::execute_inline(
                command,
                &mut self.sessions,
                &self.config.channels_dir,
            );
            self.handle_worktree_cleanup(&events);
            self.apply_events(&events).await;
            self.auto_assign_tasks(&events).await;
        }
        CommandClass::Background => {
            self.dispatch_background(command).await;
        }
        CommandClass::NeedsResolution => {
            if let Command::NudgeAgent { id, message } = command {
                self.dispatch_nudge(&id, &message).await;
            }
        }
    }
}
```

- [ ] **Step 3: Create `dispatch_background` method**

```rust
async fn dispatch_background(&mut self, command: Command) {
    match command {
        Command::SpawnAgent(mut config) => {
            self.prepare_worktree_for_spawn(&mut config);
            let agent_id = uuid::Uuid::new_v4().to_string();
            self.lifecycle_guard.mark_spawning(agent_id.clone());
            executor::spawn_background_agent(
                config,
                self.paths.clone(),
                self.config.channels_dir.clone(),
                self.event_tx.clone(),
                self.result_tx.clone(),
            );
        }
        Command::ResumeAgent { id } => {
            let agent = {
                let proj = self.projections.lock().await;
                proj.agents.by_id.get(&id).cloned()
            };
            if let Some(agent) = agent {
                self.lifecycle_guard.mark_spawning(id.clone());
                executor::spawn_background_resume(
                    id,
                    agent,
                    self.paths.clone(),
                    self.result_tx.clone(),
                );
            }
        }
        Command::StopAgent { id, reason } => {
            if let Some(session) = self.sessions.remove(&id) {
                self.lifecycle_guard.mark_spawning(id.clone());
                executor::spawn_background_stop(
                    id, reason, session, self.result_tx.clone(),
                );
            }
        }
        Command::PollPrs => {
            let work = {
                let proj = self.projections.lock().await;
                proj.work.clone()
            };
            executor::spawn_background_poll_prs(work, self.result_tx.clone());
        }
        Command::MergePr { .. }
        | Command::PostPrComment { .. }
        | Command::RerunCi { .. } => {
            executor::spawn_background_gh_command(command, self.result_tx.clone());
        }
        other => {
            // Fallback: execute inline
            let events = executor::execute_inline(
                other,
                &mut self.sessions,
                &self.config.channels_dir,
            );
            self.apply_events(&events).await;
        }
    }
}
```

- [ ] **Step 4: Create `dispatch_nudge` method**

```rust
async fn dispatch_nudge(&mut self, id: &str, message: &str) {
    // Check if agent has in-flight lifecycle operation
    if self.lifecycle_guard.is_pending(id) {
        self.lifecycle_guard.stash_nudge(id, message.to_string());
        return;
    }

    let action = {
        let proj = self.projections.lock().await;
        executor::resolve_nudge_action(id, &proj)
    };

    match action {
        executor::NudgeAction::Deliver => {
            // Inline: pipe write
            if let Some(session) = self.sessions.get_mut(id) {
                if let Err(e) = session.send_message(message).await {
                    tracing::error!(%id, %e, "failed to deliver nudge");
                }
            }
        }
        executor::NudgeAction::ResumeAndDeliver { .. } => {
            // Background: resume + deliver
            let agent = {
                let proj = self.projections.lock().await;
                proj.agents.by_id.get(id).cloned()
            };
            if let Some(agent) = agent {
                self.lifecycle_guard.mark_spawning(id.to_string());
                self.lifecycle_guard.stash_nudge(id, message.to_string());
                executor::spawn_background_resume(
                    id.to_string(), agent, self.paths.clone(), self.result_tx.clone(),
                );
            }
        }
        executor::NudgeAction::RespawnAndDeliver { config } => {
            // Background: respawn + deliver
            self.lifecycle_guard.mark_spawning(id.to_string());
            self.lifecycle_guard.stash_nudge(id, message.to_string());
            executor::spawn_background_agent(
                *config,
                self.paths.clone(),
                self.config.channels_dir.clone(),
                self.event_tx.clone(),
                self.result_tx.clone(),
            );
        }
        executor::NudgeAction::Drop => {
            tracing::debug!(%id, "nudge target unknown, dropping");
        }
    }
}
```

- [ ] **Step 5: Add result_rx arm to the select! loop and handle results**

Add this arm to the `tokio::select!` block:

```rust
Some(result) = self.result_rx.recv() => {
    match result {
        executor::ExecutorResult::Events(events) => {
            self.apply_events(&events).await;
        }
        executor::ExecutorResult::SessionReady { id, session, events } => {
            self.sessions.insert(id.clone(), session);
            self.apply_events(&events).await;
            self.auto_assign_tasks(&events).await;
            // Deliver stashed nudges
            let stashed = self.lifecycle_guard.complete(&id);
            for msg in stashed {
                if let Some(s) = self.sessions.get_mut(&id) {
                    if let Err(e) = s.send_message(&msg).await {
                        tracing::error!(%id, %e, "failed to deliver stashed nudge");
                    }
                }
            }
        }
        executor::ExecutorResult::LifecycleComplete { id, events } => {
            self.apply_events(&events).await;
            // Deliver stashed nudges — agent is stopped, so these will
            // trigger resume/respawn on next dispatch
            let stashed = self.lifecycle_guard.complete(&id);
            for msg in stashed {
                self.dispatch_nudge(&id, &msg).await;
            }
        }
    }
}
```

- [ ] **Step 6: Replace all `executor::execute()` calls with `self.dispatch_command()`**

In the RPC handler arm, scheduler arm, and web command arm, replace:
```rust
let events = executor::execute(command, &mut self.sessions, ...).await;
self.apply_events(&events).await;
```
with:
```rust
self.dispatch_command(command).await;
```

- [ ] **Step 7: Extract `auto_assign_tasks` helper**

The current code checks for `AgentCreated` with `task_id` and emits `TaskAssigned`. Extract this into a method:

```rust
async fn auto_assign_tasks(&mut self, events: &[DomainEvent]) {
    let mut assign_events = Vec::new();
    for event in events {
        if let DomainEvent::AgentCreated { id, task_id: Some(tid), .. } = event {
            assign_events.push(DomainEvent::TaskAssigned {
                task_id: tid.clone(),
                agent_id: id.clone(),
            });
        }
    }
    if !assign_events.is_empty() {
        self.apply_events(&assign_events).await;
    }
}
```

- [ ] **Step 8: Run full test suite**

Run: `cargo test`
Expected: All pass

- [ ] **Step 9: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: Clean

- [ ] **Step 10: Commit**

```bash
git add src/daemon_v2/daemon.rs src/daemon_v2/executor/mod.rs
git commit -m "feat(daemon-v2): non-blocking executor — background spawn/stop/poll with lifecycle guard"
```

---

### Task 6: Remove old `execute()` function and clean up

**Files:**
- Modify: `src/daemon_v2/executor/mod.rs`

- [ ] **Step 1: Remove the old `execute()` function**

Delete the `pub async fn execute(...)` function and all its helper functions that are now superseded:
- `execute()` — replaced by `execute_inline` + background spawn functions
- `execute_spawn()` — replaced by `spawn_background_agent`
- `execute_resume()` — replaced by `spawn_background_resume`
- `execute_nudge()` — replaced by `dispatch_nudge` on DaemonV2
- `deliver_nudge()` — inlined into `dispatch_nudge` and `SessionReady` handler

Keep:
- `resolve_nudge_action()` — still used by `dispatch_nudge`
- `spawn_config_from_agent()` — still used by background resume
- `drain_session_output()` — still used by `spawn_background_agent`
- `flush_auto_output()` — still used by drain loop
- `classify_command()`, `execute_inline()`, `LifecycleGuard` — the new API
- All `spawn_background_*` functions

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: All pass

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: Clean

- [ ] **Step 4: Commit**

```bash
git add src/daemon_v2/executor/mod.rs
git commit -m "refactor(executor): remove old blocking execute() — all commands now dispatch via classify"
```

---

### Task 7: Integration test and smoke test

- [ ] **Step 1: Run full test suite**

```bash
cargo test
```

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 3: Build release and install**

```bash
cargo install --path .
```

- [ ] **Step 4: Restart daemon and smoke test**

```bash
midtown stop
rm -f ~/.midtown/projects/midtown/events/*
MIDTOWN_DAEMON_V2=1 midtown start
midtown status
# Post to a channel with no lead — should spawn within 5s
midtown channel post --channel web "test: respond please"
# Check for response within 15s
sleep 15 && midtown channel read --channel web --last 3
```

- [ ] **Step 5: Run Playwright E2E tests**

```bash
cd web-app && npx playwright test e2e/daemon-v2-live.spec.js --reporter=list
```

- [ ] **Step 6: Final commit if any fixups needed**
