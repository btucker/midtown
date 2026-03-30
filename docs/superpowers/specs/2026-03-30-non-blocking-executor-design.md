# Non-Blocking Executor Design

## Problem

The v2 daemon runs a single `tokio::select!` loop handling RPC connections, web commands, webhooks, and scheduler ticks. When the executor runs a slow command (PollPrs HTTP calls, worktree creation, gh CLI subprocesses), the entire loop blocks. A user posting a message while PollPrs is running waits 2-5 seconds before their SpawnAgent command even starts executing. End-to-end latency for a channel lead response can be 3+ minutes.

## Architecture

Default to background execution. Only whitelist commands as inline that are provably fast and need `&mut sessions`.

### Inline Commands (main loop, needs sessions or is pure event)

| Command | Why inline |
|---------|-----------|
| NudgeAgent::Deliver | Pipe write, must be immediate |
| PollProcessHealth | `try_wait()` is non-blocking, needs `&mut sessions` to remove dead |
| AssignTask | Pure event emission |
| CompleteTask | Pure event emission |
| ResetTask | Pure event emission |
| GarbageCollect | Pure event emission |
| StopAgent (session removal) | Remove from map is instant — kill is backgrounded |

### Background Commands (tokio::spawn, results via channel)

| Command | What runs in background |
|---------|----------------------|
| SpawnAgent | Worktree creation + process spawn + drain loop setup |
| ResumeAgent | Process spawn |
| NudgeAgent (Resume/Respawn) | Process spawn + nudge delivery |
| StopAgent (kill) | Process kill + wait for exit |
| PollPrs | HTTP calls to GitHub API |
| MergePr | `gh pr merge` subprocess |
| PostPrComment | `gh pr comment` subprocess |
| RerunCi | `gh run rerun` subprocess |

### Result Channel

Background tasks send results back to the main loop via an mpsc channel:

```rust
enum ExecutorResult {
    /// Events to apply (PR polling, merge, comment, etc.)
    Events(Vec<DomainEvent>),
    /// A new session is ready — main loop inserts into sessions map
    SessionReady {
        id: AgentId,
        session: HeadlessSession,
        events: Vec<DomainEvent>,
    },
    /// Queued nudges should be delivered now (lifecycle operation completed)
    LifecycleComplete {
        id: AgentId,
        events: Vec<DomainEvent>,
    },
}
```

The main loop adds `result_rx.recv()` as an arm in `tokio::select!`. When a result arrives:

- `Events` → apply to store + projections + broadcast
- `SessionReady` → insert session into map, apply events, deliver any stashed nudges
- `LifecycleComplete` → apply events, deliver stashed nudges (may trigger resume/respawn)

### Lifecycle Guard

A single map tracks agents with in-flight lifecycle operations:

```rust
pending_lifecycle: HashMap<AgentId, Vec<String>>  // agent_id → queued nudge messages
```

**On SpawnAgent submitted to background:** Insert agent ID with empty vec.

**On StopAgent submitted to background:** Insert agent ID with empty vec.

**On NudgeAgent where target is in `pending_lifecycle`:** Push the nudge message onto the vec instead of executing.

**On SessionReady/LifecycleComplete received:** Remove from `pending_lifecycle`. For each stashed nudge message, deliver via `session.send_message()` (for SessionReady) or re-evaluate with `resolve_nudge_action` (for LifecycleComplete after stop — agent is now stopped, so this triggers resume/respawn).

**Channel-level dedup for leadless spawns:** When `route_message` produces a `SpawnAgent` for a channel with no lead, the daemon checks if that channel already has a pending spawn (by scanning `pending_lifecycle` for agents in that channel). If yes, stash the message content instead of submitting a second SpawnAgent.

### Event Loop Changes

Current:
```
select! {
    rpc = listener.accept() => { ... execute commands ... }
    webhook = webhook_rx.recv() => { ... }
    web_cmd = web_cmd_rx.recv() => { ... execute commands ... }
    sleep = timer => { ... run decisions, execute commands ... }
}
```

New:
```
select! {
    rpc = listener.accept() => { ... dispatch commands ... }
    webhook = webhook_rx.recv() => { ... }
    web_cmd = web_cmd_rx.recv() => { ... dispatch commands ... }
    result = result_rx.recv() => { ... apply results, insert sessions ... }
    sleep = timer => { ... run decisions, dispatch commands ... }
}
```

"Dispatch" means: classify command as inline or background, execute inline ones immediately, submit background ones via the background task spawner.

### What Doesn't Change

- Decision functions remain pure (`&Projections → Vec<Command>`)
- Command enum unchanged
- Projections unchanged
- `sessions: HashMap<String, HeadlessSession>` stays single-owner (no Arc/Mutex)
- The `select!` loop structure stays the same (one new arm)
- Event store persistence stays in main loop
- Worktree cleanup stays in main loop (triggered by events)

### Background Task Access

Background tasks receive owned/cloned data — no shared mutable state:

- `SpawnAgent`: owns `SpawnConfig`, `ProjectPaths` (cloneable), `channels_dir` (PathBuf), `event_tx` (broadcast sender clone)
- `PollPrs`: owns `WorkIndex` snapshot (cloned from projections)
- `StopAgent`: owns `HeadlessSession` (moved out of sessions map before backgrounding)
- CLI commands: own the arguments (number, body, run_id)

The `result_tx: mpsc::Sender<ExecutorResult>` is cloned into each background task.

### Error Handling

Background tasks that fail (spawn error, HTTP error, CLI error) send back error events via the same result channel:

- Failed spawn → `ExecutorResult::Events(vec![AgentSpawnFailed { ... }])`
- Failed PollPrs → logged, no events (same as today)
- Failed CLI command → logged, no events (same as today)
- Failed stop → `ExecutorResult::Events(vec![AgentStopFailed { ... }])`

The `pending_lifecycle` entry is removed on any result (success or failure), so stashed nudges are processed either way. On spawn failure, the stashed nudges will trigger `resolve_nudge_action` which sees no running agent and attempts respawn — the cooldown system prevents tight loops.

## Testing

- Unit test: `dispatch_command` correctly classifies inline vs background
- Unit test: lifecycle guard stashes nudges during in-flight spawn
- Unit test: lifecycle guard delivers stashed nudges on SessionReady
- Unit test: lifecycle guard handles stop → nudge → resume sequence
- Integration test: PollPrs doesn't block NudgeAgent delivery (timing-based)
