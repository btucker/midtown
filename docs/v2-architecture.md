> Back to [README](../README.md) | See also: [v1 architecture](architecture.md) | [v2 design spec](superpowers/specs/2026-03-28-daemon-v2-design.md)

# Daemon V2 Architecture

The v2 daemon (`src/daemon_v2/`) is an event-sourced rewrite. Launch with `MIDTOWN_DAEMON_V2=1 midtown start`. It coexists with v1 — same socket path, same CLI.

## Core Pipeline

```
Command  →  executor (I/O)  →  DomainEvent(s)  →  update projections
   ↑                                                       |
   +──── decision functions read projections (immutable) ──+
```

Three input sources feed the loop:
- **Scheduler** — fires decision functions on intervals (5s–1hr)
- **RPC** — JSON-RPC over Unix socket (CLI commands, web UI)
- **Webhooks** — GitHub events converted to domain events

## Module Layout

```
src/daemon_v2/
├── daemon.rs              # Event loop, scheduler wiring, worktree management
├── scheduler.rs           # Per-decision interval tracking
│
├── events/
│   ├── mod.rs             # DomainEvent enum (~20 variants)
│   └── store.rs           # Append-only JSONL log + snapshots
│
├── projections/
│   ├── mod.rs             # Projections container (apply all events)
│   ├── agents.rs          # AgentIndex: by_id, by_name, by_task, by_channel, by_thread
│   ├── work.rs            # WorkIndex: tasks + PRs with pre-indexed views
│   ├── channels.rs        # ChannelIndex: metadata, settings, thread counts
│   └── cooldowns.rs       # CooldownTracker: unified rate-limiting
│
├── decisions/
│   ├── mod.rs             # Command enum, SpawnConfig
│   ├── dispatch.rs        # Task assignment, duplicate detection
│   ├── health.rs          # Dead workers, idle shutdown, multi-channel leads
│   ├── prs.rs             # Merged PRs, reviewer spawning + escalation, rebase nudge
│   ├── chat.rs            # Message routing (thread/channel/mention/task-ref)
│   └── lifecycle.rs       # Agent GC, DM channel naming
│
├── executor/
│   ├── mod.rs             # Command → I/O → DomainEvent; resume-on-nudge
│   ├── spawn.rs           # Agent process management (HeadlessSession)
│   ├── github.rs          # PR polling via `gh` CLI
│   ├── channel_io.rs      # Channel JSONL read/write
│   └── webhook.rs         # WebhookEvent → DomainEvent conversion
│
├── rpc/
│   ├── mod.rs             # Method dispatch (v2 + v1 compatibility aliases)
│   └── handlers.rs        # Request → response/events/commands
│
└── web/
    ├── mod.rs             # Axum router
    ├── routes.rs          # HTTP REST endpoints
    └── websocket.rs       # Event broadcast to web UI
```

## Event Store

Events are appended one-per-line to `log-NNNN.jsonl`. Snapshots serialize all projections to `snapshot-NNNN.json` periodically. Recovery: load latest snapshot + replay remaining events. Partial lines from crashes are truncated on recovery.

On-disk layout:
```
~/.midtown/projects/<dir_key>/
├── daemon.sock
├── daemon.pid
├── events/
│   ├── snapshot-0042.json
│   └── log-0042.jsonl
├── channels/
│   └── <channel>/channel.jsonl
└── worktrees/
```

## Projections

Four in-memory read models, each implementing `fn apply(&mut self, event: &DomainEvent)`:

### AgentIndex

```rust
struct AgentIndex {
    by_id: HashMap<AgentId, Agent>,
    by_name: HashMap<String, AgentId>,
    by_task: HashMap<TaskId, AgentId>,
    by_channel: HashMap<String, Vec<AgentId>>,
    by_thread: HashMap<String, AgentId>,
    running: HashSet<AgentId>,
}
```

Key methods:
- `channel_lead(channel)` — find the Lead agent for a channel (prefers running; falls back to most recent stopped)
- `fork_for_thread(thread_id)` — find the agent bound to a thread
- `idle_workers()` — running workers with no task

**Thread bindings persist through stop.** `AgentStopped` removes from `running` but NOT from `by_thread`. This allows nudges to resume stopped forks. Only `AgentGarbageCollected` clears thread bindings.

### WorkIndex

Tasks and PRs combined. The PR link lives on the task — no parallel maps.

Pre-indexed views: `pending_tasks`, `in_progress_tasks`, `open_prs`, `needing_review`, `blocked`.

### ChannelIndex

Channel metadata created implicitly from `MessagePosted` events. Settings: `lead_driven`, `directory` (for AGENTS.md loading), `show_full_lead_output`.

### CooldownTracker

One mechanism replaces v1's ten ad-hoc ones. Categories: `OrphanSpawn`, `AgentDispatch`, `SpawnFailure`, `MergeRebaseNudge`, `RebaseRegression`, `LeadWorktreeFreshness`, `TaskNudge`, `NoteStaleness`. Not serialized — resets on restart (safe, all durations are short).

## Decision Functions

Pure functions: `fn(&Projections) -> Vec<Command>` (some take extra args like `default_channel` or `max_tasks`). No I/O, no mutation, no async.

### Scheduled Decisions

| Decision | Interval | Purpose |
|----------|----------|---------|
| `dispatch_pending_tasks` | 5s | Match pending tasks to idle workers |
| `stop_completed_agents` | 5s | Stop agents whose tasks are done |
| `check_dead_workers` | 30s | Reset tasks for stopped agents |
| `check_idle_workers` | 30s | Stop taskless workers after 5min |
| `check_duplicate_workers` | 30s | Stop older of two agents on same task |
| `ensure_channel_leads_alive` | 30s | Spawn leads for all active channels |
| `poll_process_health` | 10s | Detect dead processes via try_wait |
| `poll_prs` | 45s | Fetch open/merged PRs from GitHub |
| `handle_merged_prs` | 10s | Complete tasks for merged PRs |
| `spawn_reviewers` | 45s | Spawn reviewers (with escalation after 3 failures) |
| `suspend_authors_with_prs` | 10s | Stop workers whose PRs are in review |
| `garbage_collect` | 1hr | Remove agent records stopped >24hr |

### Message Routing (chat.rs)

`route_message()` is the single nudge entry point. Three binding-based rules:

1. **Thread reply** → nudge agent bound to that thread (`by_thread`). No binding → channel lead.
2. **Top-level message** → nudge channel lead (`channel_lead()`).
3. **@mentions / !N task refs** → nudge named or task-assigned agent.

Design principles:
- **Routing is by binding, not agent type.** No `AgentKind` checks except `channel_lead()`.
- **No running-state checks.** Executor resumes stopped agents.
- **Self-nudges suppressed.** Sender name matched against agent name.
- **Deduplication** via `HashSet<AgentId>` — each agent nudged at most once per message.

### Reviewer Escalation (prs.rs)

`spawn_reviewers` counts stopped reviewer agent records per PR. After `MAX_REVIEWER_RESTARTS` (3) failures, posts escalation to ops channel instead of spawning again. Relies on distinct agent IDs per spawn attempt (executor creates new ID each time).

### Rebase Nudging (prs.rs)

`nudge_rebase_after_merge` nudges workers with open PRs to rebase when any PR has merged. Uses `MergeRebaseNudge` cooldown (1hr per agent) to prevent repeated nudging.

## Executor

`execute()` in `executor/mod.rs` maps each `Command` variant to I/O and returns `DomainEvent`s.

### NudgeAgent with Resume

When `NudgeAgent` targets a stopped agent:

```rust
enum NudgeAction {
    Deliver,                              // Running — send message
    ResumeAndDeliver { session_id },      // Stopped with session — resume then send
    Drop,                                 // Unknown or no session — can't resume
}
```

`resolve_nudge_action()` determines the strategy. `resume_agent()` is shared between `NudgeAgent` and `ResumeAgent` command handling.

### Agent Spawning

`spawn_agent()` builds a `LaunchConfig` → `HeadlessConfig` → `HeadlessSession::spawn()`. DM channels are auto-created for agents without a channel/thread binding.

### Auto-Output

Agent stdout is processed in a background tokio task (`drain_session_output`). Every 2 seconds it:
1. Accumulates `StreamEvent`s from the session's stdout
2. Extracts assistant text via `extract_assistant_text()` (shared with v1's `daemon::stream`)
3. Posts the text to the agent's bound channel via `channel_io::post_message()`

This is how agent responses appear in channels — the agent writes to stdout, the daemon captures it and posts it. Without this, agents "respond" but their output is invisible.

Worktree assignment happens in `DaemonV2::prepare_worktree_for_spawn()` before the command reaches the executor:
- Workers with task_id get a task worktree (branched)
- Leads/Forks get the shared lead worktree
- Existing worktrees are reused on re-dispatch

## RPC

v2 endpoints + v1 compatibility aliases in a single dispatch table. Mutating methods return `(response, Vec<DomainEvent>, Vec<Command>)`. The daemon applies events and executes commands after sending the response.

Key v2 methods: `status`, `agent.list`, `task.create/list/done/update`, `channel.post/read/list/update`, `session.fork`, `pr.list/action`.

v1 aliases: `ping`, `version`, `snapshot`, `coworker.spawn/break/nudge/list`, `lead.spawn`, `prs.status`.

## Web API

Axum HTTP server + WebSocket. Routes map to RPC methods. WebSocket broadcasts all domain events to connected clients for real-time UI updates.

## Startup and Recovery

1. Load snapshot + replay events → projections
2. PID-check all "running" agents → emit `AgentStopped` for dead ones
3. Queue `ResumeAgent` for agents with `session_id`
4. Lock PID file, bind socket, start web server
5. Execute pending resumes
6. Enter event loop (`tokio::select!` on socket, webhooks, scheduler)

`AgentResumed` resets `started_at` so idle checks use resume time, not original spawn time.
