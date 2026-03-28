# Daemon v2: Event-Sourced Rebuild

## Problem

The current daemon (~78,000 lines across 30+ files) grew organically and suffers from systemic issues:

- **State inconsistency**: Multiple overlapping maps (SessionManager, SessionRecord, CoworkerSession) disagree about truth. 4 inconsistent PR-to-task association sources.
- **Bloated tick pipeline**: 50+ ephemeral `tick_*` fields pre-computed on every tick to maintain "pure" decision functions. The purity is real but the ergonomics are terrible.
- **Ad-hoc complexity**: 10 different cooldown mechanisms, 63 Effect variants (many redundant), 60+ RPC endpoints.
- **Tight coupling**: `mod.rs` (4,108 lines), `effects.rs` (4,601 lines), `pr.rs` (5,270 lines) — files organized by pipeline phase rather than domain concept.

## Architecture: Event Sourcing with Projection Views

Every state mutation flows through one path:

```
Command  ->  execute (I/O)  ->  DomainEvent(s)  ->  update projections
   ^                                                       |
   |                                                       v
   +---- decision functions read projections (immutable) --+
```

### DomainEvent

A flat enum describing what happened (~25 variants):

```rust
enum DomainEvent {
    // Agents
    AgentCreated {
        id: AgentId, name: String, kind: AgentKind, agent_type: String,
        provider: Provider, channel: Option<String>, task_id: Option<TaskId>,
        bound_thread_id: Option<String>,  // Fork thread binding
        session_id: Option<String>,       // Claude Code session ID (for --fork-session)
    },
    AgentStarted { id: AgentId, pid: u32 },
    AgentStopped { id: AgentId, reason: String },
    AgentResumed { id: AgentId },

    // Tasks
    TaskCreated { id: TaskId, subject: String, channel: String, blocked_by: Vec<TaskId> },
    TaskAssigned { task_id: TaskId, agent_id: AgentId },
    TaskCompleted { task_id: TaskId },
    TaskReset { task_id: TaskId, reason: String },
    TaskUnblocked { task_id: TaskId },

    // PRs
    PrOpened { number: u64, branch: String, author: String },
    PrUpdated { number: u64, ci_status: CiStatus, review_state: ReviewState },
    PrMerged { number: u64, branch: String },
    PrClosed { number: u64 },
    PrReviewRequested { number: u64 },
    PrLinkedToTask { number: u64, task_id: TaskId },

    // Chat
    MessagePosted { id: MessageId, channel: String, sender: String, content: String, thread_id: Option<String> },
    MentionRouted { message_id: MessageId, target_agent: AgentId },

    // Health
    ProcessHealthChecked { agent_id: AgentId, status: ProcessStatus },
    UsageLimitHit { agent_id: AgentId, reset_at: DateTime<Utc> },
    AuthErrorDetected { agent_id: AgentId },

    // Worktrees
    WorktreeCreated { id: WorktreeId, path: PathBuf, task_id: Option<TaskId> },
    WorktreeRemoved { id: WorktreeId },

    // Channel settings
    ChannelLeadDrivenSet { channel: String, lead_driven: bool },
    ChannelDirectorySet { channel: String, directory: Option<String> },

    // Config
    ConfigUpdated { key: String, value: serde_json::Value },
}
```

Events describe facts, not intent. They are the single source of truth.

### Agent Taxonomy

- **Agent** — any managed CLI process (the generic term)
- **Lead** — agent bound to a channel (human-facing)
- **Fork** — agent bound to a thread (research session)
- **Worker** — agent bound to a task (does work)

```rust
enum AgentKind { Lead, Fork, Worker }

enum Provider { ClaudeCode, Codex }

struct Agent {
    id: AgentId,
    name: String,
    kind: AgentKind,
    agent_type: String,          // e.g. "midtown-code-author"
    provider: Provider,
    session_id: Option<String>,  // Claude Code session ID (for resume/fork)
    channel: Option<String>,
    task_id: Option<TaskId>,
    bound_thread_id: Option<String>,  // Fork thread binding
    pid: Option<u32>,
    started_at: Option<DateTime<Utc>>,
    stopped_at: Option<DateTime<Utc>>,
}
```

"Session" is an implementation detail (the process/resume ID), not a user-facing concept.

## Projections

Three materialized views, each implementing `fn apply(&mut self, event: &DomainEvent)`:

### AgentIndex

```rust
struct AgentIndex {
    by_id: HashMap<AgentId, Agent>,
    by_name: HashMap<String, AgentId>,
    by_task: HashMap<TaskId, AgentId>,
    by_channel: HashMap<String, Vec<AgentId>>,
    by_thread: HashMap<String, AgentId>,  // thread_id → fork agent
    running: HashSet<AgentId>,
}
```

### WorkIndex

Tasks and PRs combined. The PR link lives on the task — no parallel maps.

```rust
struct WorkIndex {
    tasks: HashMap<TaskId, Task>,
    prs: HashMap<u64, PrState>,

    // Pre-indexed views (rebuilt by apply())
    pending_tasks: Vec<TaskId>,
    in_progress_tasks: Vec<TaskId>,
    open_prs: Vec<u64>,
    needing_review: Vec<u64>,
    blocked: HashMap<TaskId, Vec<TaskId>>,
}

struct Task {
    id: TaskId,
    subject: String,
    channel: String,
    status: TaskStatus,
    pr_number: Option<u64>,     // Single source of truth for task<->PR link
    blocked_by: Vec<TaskId>,
    agent_type: Option<String>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

impl WorkIndex {
    fn pr_for_task(&self, id: &TaskId) -> Option<&PrState> {
        self.tasks.get(id)?.pr_number.and_then(|n| self.prs.get(&n))
    }

    fn task_for_pr(&self, pr: u64) -> Option<(&TaskId, &Task)> {
        self.tasks.iter().find(|(_, t)| t.pr_number == Some(pr))
    }

    fn pending_unblocked(&self) -> Vec<&TaskId> {
        self.pending_tasks.iter()
            .filter(|id| !self.blocked.contains_key(id))
            .collect()
    }
}
```

### ChannelIndex

```rust
struct ChannelIndex {
    channels: HashMap<String, ChannelMeta>,
    read_state: HashMap<String, DateTime<Utc>>,
}

struct ChannelMeta {
    name: String,
    archived: bool,
    settings: ChannelSettings,
    workflow: Option<String>,
    thread_count: usize,
    last_message_at: Option<DateTime<Utc>>,
    known_threads: HashSet<String>,  // Dedup thread counting
}

struct ChannelSettings {
    show_full_lead_output: bool,
    lead_driven: bool,            // Skip auto-dispatch for this channel
    directory: Option<String>,    // Repo subdirectory for AGENTS.md loading
}
```

Message content stays in channel JSONL files — not duplicated into the event log or projections.

**Lead-driven mode**: When `lead_driven: true`, the dispatch decision skips tasks in this channel. The lead manages work manually via RPC. Workflow events are forwarded as @mentions to the lead instead of being executed automatically.

**Channel directory**: The `directory` field specifies a subdirectory of the repo (e.g., `"packages/auth"`). When the daemon spawns a lead for this channel, it passes the directory as `working_dir` so the lead loads `AGENTS.md`/`CLAUDE.md` from that subdirectory, giving it domain-specific context.

### CooldownTracker

One mechanism replaces ten ad-hoc ones:

```rust
struct CooldownTracker {
    entries: HashMap<(CooldownCategory, String), Instant>,
}

enum CooldownCategory {
    OrphanSpawn,
    AgentDispatch,
    SpawnFailure,
    MergeRebaseNudge,
    RebaseRegression,
    LeadWorktreeFreshness,
    TaskNudge,
    NoteStaleness,
}

impl CooldownCategory {
    fn duration(&self) -> Duration {
        match self {
            Self::OrphanSpawn => Duration::from_secs(60),
            Self::AgentDispatch => Duration::from_secs(30),
            Self::SpawnFailure => Duration::from_secs(120),
            Self::MergeRebaseNudge => Duration::from_secs(3600),
            Self::RebaseRegression => Duration::from_secs(3600),
            Self::LeadWorktreeFreshness => Duration::from_secs(300),
            Self::TaskNudge => Duration::from_secs(3600),
            Self::NoteStaleness => Duration::from_secs(3600),
        }
    }
}

impl CooldownTracker {
    fn is_active(&self, category: CooldownCategory, key: &str) -> bool {
        self.entries.get(&(category, key.into()))
            .map(|t| t.elapsed() < category.duration())
            .unwrap_or(false)
    }

    fn record(&mut self, category: CooldownCategory, key: String) {
        self.entries.insert((category, key), Instant::now());
    }
}
```

### Projections Container

```rust
struct Projections {
    agents: AgentIndex,
    work: WorkIndex,
    channels: ChannelIndex,
    cooldowns: CooldownTracker,
}

impl Projections {
    fn apply(&mut self, event: &DomainEvent) {
        self.agents.apply(event);
        self.work.apply(event);
        self.channels.apply(event);
    }
    // Cooldowns are not event-driven — they use wall-clock Instants.
    // They are not serialized into snapshots; on restart all cooldowns reset (safe — they're short-lived).
}
```

## Decisions and Commands

Decision functions take immutable projection references, return Commands:

```rust
enum Command {
    // Agent lifecycle
    SpawnAgent(SpawnConfig),
    StopAgent { id: AgentId, reason: String },
    ResumeAgent { id: AgentId },
    NudgeAgent { id: AgentId, message: String },

    // Work management
    AssignTask { task_id: TaskId, agent_id: AgentId },
    CompleteTask { task_id: TaskId },
    ResetTask { task_id: TaskId },
    CreateReviewTask { pr_number: u64, channel: String },

    // Communication
    Post { channel: String, sender: String, content: String, thread_id: Option<String> },
    PostSystem { channel: String, content: String },
    Broadcast(BroadcastUpdate),

    // Worktree
    CreateWorktree { task_id: TaskId, branch: String },
    RemoveWorktree { task_id: TaskId },

    // GitHub
    MergePr { number: u64 },
    RerunCi { run_id: u64 },
    PostPrComment { number: u64, body: String },

    // Polling (executor performs I/O, emits events)
    PollPrs,
    PollProcessHealth,
}
```

~15 variants instead of 63. Redundant specificity is gone — `SpawnAgent` with a config struct replaces `SpawnCoworker`, `SpawnCoworkerWithCallbacks`, `SpawnForTask`, and `ResumeCoworker`.

### Decision Functions

Plain functions grouped by domain. No traits, no framework:

```rust
// decisions/dispatch.rs
fn dispatch_pending_tasks(proj: &Projections, config: &DaemonConfig) -> Vec<Command> {
    if proj.cooldowns.is_active(CooldownCategory::AgentDispatch, "global") {
        return vec![];
    }

    let available = proj.agents.idle_workers();
    let pending = proj.work.pending_unblocked();

    pending.iter()
        .zip(available.iter())
        .map(|(task_id, agent_id)| Command::AssignTask {
            task_id: (*task_id).clone(),
            agent_id: *agent_id,
        })
        .collect()
}
```

No locks. No `&mut`. No tick fields. No async. Trivially testable — construct projections, call function, assert commands.

### Decision Inventory

All decision functions, mapped from current code:

**dispatch.rs** (~200 lines, down from 2,289):
- `dispatch_pending_tasks` — match pending tasks to idle workers
- `stop_completed_sessions` — stop agents whose tasks are done
- `reset_orphaned_tasks` — reset tasks with no living agent
- `check_duplicate_workers` — detect two agents on one task

**health.rs** (~250 lines, down from 1,461):
- `check_process_health` — detect dead processes, trigger respawn
- `check_idle_workers` — shut down workers with no task activity
- `check_auth_errors` — detect and handle auth failures
- `check_usage_limits` — detect rate limits, schedule retry
- `ensure_leads_alive` — respawn dead lead/channel-lead agents
- `garbage_collect` — remove long-dead agent records, stale worktrees

**prs.rs** (~300 lines, down from 5,270):
- `reconcile_pr_state` — compare polled PR data with WorkIndex, emit updates
- `spawn_reviewers` — create review tasks for PRs needing review
- `handle_merged_prs` — complete tasks, clean worktrees for merged PRs
- `check_ci_issues` — detect stale checks, rerun failures
- `check_rate_limits` — monitor GitHub API quota

**chat.rs** (~100 lines):
- `route_mention` — @mention → target agent resolution

**lifecycle.rs** (~100 lines):
- `fire_reminders` — check cron/event triggers
- `cleanup_worktrees` — remove stale/orphaned worktrees

### I/O Boundary: Polling vs Decisions

Some scheduled work requires I/O before decisions can run (e.g., `gh pr list` to get current PR state, `kill -0` to check PIDs). This I/O is **not** part of the decision function. It lives in the **executor** as a "polling command":

```rust
enum Command {
    // ... existing variants ...

    // Polling commands — executor performs I/O, emits events, no further commands needed
    PollPrs,
    PollProcessHealth,
    PollRateLimits,
}
```

The scheduler fires `PollPrs` every 45s. The executor calls `gh pr list`, diffs against the current WorkIndex, and emits `PrOpened`/`PrUpdated`/`PrMerged` events. The projections update. Then the *next* scheduled run of `spawn_reviewers` or `handle_merged_prs` sees the new state and makes pure decisions.

This keeps decisions pure while acknowledging that some scheduled work is "gather data" not "make decisions."

## Tick Scheduling

No tick types. Each decision function declares its own interval:

```rust
struct ScheduledDecision {
    name: &'static str,
    interval: Duration,
    run: fn(&Projections, &DaemonConfig) -> Vec<Command>,
}

const DECISIONS: &[ScheduledDecision] = &[
    // Fast (5s)
    sd("dispatch_pending_tasks",    5,  dispatch::dispatch_pending_tasks),
    sd("stop_completed_sessions",   5,  dispatch::stop_completed_sessions),

    // Medium (30s)
    sd("check_process_health",      30, health::check_process_health),
    sd("check_idle_workers",        30, health::check_idle_workers),
    sd("check_auth_errors",         30, health::check_auth_errors),
    sd("ensure_leads_alive",        30, health::ensure_leads_alive),

    // Slow (45s-2min)
    sd("reconcile_pr_state",        45, prs::reconcile_pr_state),
    sd("spawn_reviewers",           45, prs::spawn_reviewers),
    sd("handle_merged_prs",         45, prs::handle_merged_prs),
    sd("check_rate_limits",         120, prs::check_rate_limits),

    // Rare (hourly)
    sd("garbage_collect",           3600, health::garbage_collect),
    sd("fire_reminders",            60,   lifecycle::fire_reminders),
    sd("cleanup_worktrees",         3600, lifecycle::cleanup_worktrees),
];
```

### Main Loop

Three input sources, one output path:

```rust
loop {
    tokio::select! {
        Some(webhook) = webhook_rx.recv() => {
            let events = handle_webhook(webhook, &projections);
            apply_and_execute(events);
        }

        Some(rpc) = rpc_rx.recv() => {
            let response = handle_rpc(rpc, &projections, &config);
            rpc.reply(response);
        }

        decision = scheduler.next() => {
            let commands = (decision.run)(&projections, &config);
            let commands = deduplicate(commands);
            for cmd in commands {
                let events = execute(cmd).await;
                projections.apply_all(&events);
                store.append_all(&events);
                broadcast_ws(&events);
            }
        }
    }
}
```

Webhooks can trigger decision functions immediately — no waiting for the next poll tick.

## RPC Consolidation

~15 endpoints replacing 60+, organized as CRUD on 6 resources:

```
status              — aggregated daemon status (agent/task/PR counts)

agent.list          — query AgentIndex with filters (kind, running_only)
agent.spawn         — Command::SpawnAgent
agent.stop          — Command::StopAgent
agent.nudge         — Command::NudgeAgent (send_message to running session)

session.fork        — fork a thread: spawn Fork agent with --fork-session
                      from parent lead's session_id. Deduplicates by thread.

task.list           — query WorkIndex with filters
task.create         — emit TaskCreated event
task.update         — update task fields
task.action         — done/claim/prompt/handoff

channel.list        — list channels from filesystem
channel.create      — create channel directory + event
channel.update      — settings: lead_driven, directory, archive/unarchive
channel.post        — write message to channel JSONL + emit MessagePosted
channel.read        — read channel JSONL with optional limit

pr.list             — query WorkIndex PR data
pr.action           — review/merge/allow/rerun

reminder.list       — query reminders
reminder.create     — create reminder
reminder.delete     — delete reminder

config.get          — read config
config.update       — update config (auth switch, pool toggle)

shutdown            — graceful daemon shutdown
```

### Endpoint Mapping (current -> new)

| Current | New |
|---|---|
| `coworker.spawn`, `lead.spawn` | `agent.spawn` (SpawnConfig distinguishes kind) |
| `coworker.break`, `coworker.stop_all` | `agent.stop` (filter param for "all") |
| `coworker.nudge`, `coworker.asking` | `agent.nudge` |
| `coworker.list`, `coworker.view`, `coworkers.status`, `coworker.report-state`, `coworker.questions` | `agent.list` (filter + detail level) |
| `task.create/update/done/claim/metadata/request/prompt/handoff` | `task.create`, `task.update`, `task.action` |
| `channel.post/read/list/create/archive/unarchive/rename/get_settings/set_settings` | `channel.list/create/update/post/read` |
| `pr.review/merge/auto-merge/review-post/list-external/allow` | `pr.list`, `pr.action` |
| `read_state.get/mark_read` | folded into `channel.read` |
| `workflow.*` (6 endpoints) | `channel.update` (workflow is a channel property) |
| `auth.switch/pool-toggle` | `config.update` |
| `status/ping/version/snapshot` | `status` (detail param) |

The RPC handler is mechanical — each match arm is 5-10 lines that either queries a projection or translates params to a Command:

```rust
async fn handle_rpc(req: RpcRequest, proj: &Projections, config: &DaemonConfig) -> RpcResponse {
    match req.method.as_str() {
        "agent.list" => {
            let filter: AgentFilter = parse_params(req.params)?;
            Ok(json!(proj.agents.query(&filter)))
        }
        "task.action" => {
            let params: TaskActionParams = parse_params(req.params)?;
            let cmd = params.to_command()?;
            let events = execute(cmd).await?;
            Ok(json!({ "ok": true }))
        }
        _ => Err(RpcError::MethodNotFound),
    }
}
```

The web API maps 1:1 onto the same RPC methods. HTTP routing is just URL → method name translation.

## State Persistence and Recovery

### On-Disk Layout

```
~/.midtown/projects/<dir_key>/
├── daemon.sock
├── daemon.pid
├── events/
│   ├── snapshot-0042.json      # Full projection state at event #4200
│   └── log-0042.jsonl          # Events since snapshot (append-only)
├── channels/
│   └── <channel>/channel.jsonl # Unchanged — still append-only JSONL
└── worktrees/
```

`tasks.json` is gone — tasks live in the WorkIndex projection, persisted via snapshots.

### Write Path

Events are appended one-per-line to `log-NNNN.jsonl`. Crash-safe: a partial line on crash is truncated on recovery.

### Snapshot Strategy

Every N events (e.g., 1000), serialize all projections to `snapshot-NNNN.json`, start a new log file. Old files can be deleted.

### Recovery

```rust
fn recover(dir: &Path) -> (EventStore, Projections) {
    // 1. Load latest snapshot
    let snapshot = load_latest_snapshot(dir);

    // 2. Replay events since snapshot
    let events = read_events_since(dir, snapshot.sequence);

    // 3. Rebuild projections
    let mut proj = snapshot.projections;
    for event in &events {
        proj.apply(event);
    }

    // 4. Reconcile with reality (processes may have died during downtime)
    let health = check_all_processes(&proj.agents);
    let fixup_events = reconcile(health, &proj);
    for event in &fixup_events {
        proj.apply(event);
    }

    (store, proj)
}
```

## Module Structure

```
src/daemon/
├── mod.rs                  # ~100 lines — tokio::select loop
├── config.rs               # DaemonConfig
│
├── events/
│   ├── mod.rs              # DomainEvent enum
│   ├── store.rs            # Append-only log, snapshot, recovery
│   └── reconcile.rs        # Post-crash process reconciliation
│
├── projections/
│   ├── mod.rs              # Projections container
│   ├── agents.rs           # AgentIndex
│   ├── work.rs             # WorkIndex (tasks + PRs)
│   ├── channels.rs         # ChannelIndex
│   └── cooldowns.rs        # CooldownTracker
│
├── decisions/
│   ├── mod.rs              # Command enum
│   ├── dispatch.rs         # Task assignment, orphan recovery
│   ├── health.rs           # Process health, idle detection, auth errors
│   ├── prs.rs              # PR monitoring, review spawning, CI
│   ├── chat.rs             # @mention routing
│   └── lifecycle.rs        # GC, worktree cleanup, reminders
│
├── executor/
│   ├── mod.rs              # Command -> I/O -> DomainEvent(s)
│   ├── spawn.rs            # Agent process management
│   ├── github.rs           # gh CLI calls, webhook handling
│   ├── channel_io.rs       # Channel JSONL writes
│   └── worktree.rs         # Git worktree operations
│
├── rpc/
│   ├── mod.rs              # Method dispatch (~15 endpoints)
│   └── handlers.rs         # Query/command translation
│
├── web/
│   ├── mod.rs              # Axum router
│   ├── routes.rs           # HTTP -> RPC translation
│   └── websocket.rs        # Event stream -> WS broadcast
│
└── scheduler.rs            # Timer wheel for scheduled decisions
```

**File size target:** No file over 500 lines.

**Dependency flow** (strictly one-directional):

```
config
  |
  v
events  <---------------------+
  |                            |
  v                            |
projections                    |
  |                            |
  v                            |
decisions  ->  Command         |
  |                            |
  v                            |
executor   ->  DomainEvent  ---+
  |
  v
rpc / web (read projections, submit commands)
```

No circular dependencies. The only cycle is intentional: executor produces events that feed back into projections.

**Test structure** follows existing convention (separate test files with `#[path]` inclusion). Decision functions are trivially testable — construct projections, call function, assert commands. No mocks, no async, no I/O.

## Migration Strategy

Build alongside the old daemon. Migrate one domain at a time.

### Phase 1: Skeleton + Event Store + RPC
- New daemon module (feature-flagged or separate binary)
- Event store with snapshot/recovery
- Empty projections
- RPC server with `status` and `agent.list`
- **Gate:** `midtown status` works against new daemon

### Phase 2: Agent Lifecycle
- Port spawning, stopping, resuming
- AgentIndex projection
- Decision functions: `check_process_health`, `check_idle_workers`
- Reuse existing `launch.rs` and `headless.rs`
- **Gate:** Can spawn a lead and workers, detect dead processes

### Phase 3: Task Dispatch
- WorkIndex projection (tasks only)
- Decision functions: `dispatch_pending_tasks`, `stop_completed_sessions`
- Task CRUD via RPC
- **Gate:** `task create` -> worker spawns -> completes -> worktree cleaned

### Phase 4: PR Monitoring
- Add PRs to WorkIndex
- Webhook handler
- Decision functions: `reconcile_pr_state`, `spawn_reviewers`
- **Gate:** PR opened -> review task -> reviewer spawns -> posts comment

### Phase 5: Chat + Channels
- ChannelIndex projection
- Chat monitor (reuse existing tailf approach)
- @mention routing
- Channel CRUD via RPC
- **Gate:** Full channel system, web UI functional

### Phase 6: Cutover
- Feature parity test
- Swap binary
- Remove old daemon code

### What to Reuse vs Rewrite

| Reuse | Rewrite |
|---|---|
| `launch.rs` / `headless.rs` (agent process mgmt) | `daemon/mod.rs` (4,108 -> ~100 lines) |
| `channel.rs` (JSONL read/write) | `state.rs` (-> event store + projections) |
| `webhook.rs` (HTTP handler) | `effects.rs` (4,601 lines -> executor/) |
| `worktree.rs` (git operations) | `pr.rs` (5,270 lines -> decisions/prs.rs) |
| `web.rs` / `webserver.rs` (routes) | `dispatch.rs` / `health.rs` (-> decisions/) |
| `message.rs` (message types) | All `rpc_*.rs` (-> single handlers.rs) |

### Expected Size

~15,000-20,000 lines (down from ~78,000). Reductions:
- 63 effects -> 15 commands
- 50+ tick fields -> 0 (projections replace them)
- 10 cooldown mechanisms -> 1
- 60+ RPC endpoints -> 15
- 6 rpc_*.rs files -> 1 handlers.rs
- No duplicate state maps

## Implementation Status and Remaining Gaps

*Updated 2026-03-28 after Phases 1-6 implementation.*

### What's Built (84 unit tests, 10 E2E tests, all green)

- **Event store** with JSONL append, snapshots, crash-safe recovery
- **Projections**: AgentIndex (by_id/name/task/channel/thread), WorkIndex (tasks+PRs combined), ChannelIndex (settings, directory, lead_driven), CooldownTracker
- **Decisions**: dispatch_pending_tasks, stop_completed_agents, check_dead_workers, ensure_leads_alive, handle_merged_prs
- **Executor**: real HeadlessSession spawning, process health polling via try_wait(), channel JSONL I/O, GitHub PR polling via `gh` CLI
- **RPC**: status, agent.list, task.create, session.fork, channel.post/read/list/update, shutdown
- **Scheduler**: per-decision intervals, timer wheel
- **Lead-driven workflows**: channels with lead_driven flag skip auto-dispatch
- **Channel directories**: channel.update with directory setting, passed as working_dir to leads
- **Thread forks**: session.fork RPC, by_thread index, fork deduplication, MIDTOWN_BOUND_THREAD_ID env var
- **CLI**: `midtown daemon-v2 --socket --workdir --channel`
- **E2E tests**: startup, status, agent list, task create, agent spawn, lead-driven skip, channel post/read, PR polling, thread fork, shutdown

### Recently Fixed

- **Fork context**: Forks now use `--fork-session` with the parent lead's session_id. E2E verified. Agent.session_id is tracked via AgentStarted events.
- **NudgeAgent**: Executor calls `session.send_message()` on the HeadlessSession.
- **ResumeAgent**: Emits AgentResumed event. Full HeadlessConfig-based resume deferred.
- **Sandboxing**: All spawned agents are sandboxed via `sandbox-exec` on macOS (inherited from HeadlessSession::spawn).
- **`--agent` on forks**: Fork sessions get `--agent midtown-channel-lead` for agent definition loading.

### Remaining Gaps

#### Tier 1: Required for Functional Parity

**Startup reconciliation**: Recovered events can leave agents marked "running" with no live process. On daemon startup, must check PIDs and emit AgentStopped for dead agents. Without this, `ensure_leads_alive` thinks a lead exists when it doesn't, and forks can't find a valid session_id.

**PR review lifecycle**: The core "work gets done" loop:
- Auto-create review task when PR is opened/review-requested
- Suspend author worker when PR opens (save session_id for later resume)
- Nudge author with review feedback
- Resume author with `--resume <session_id>` and review context as initial prompt
- Merge confirmation flow

Requires: `CreateReviewTask` command, full `ResumeAgent` wiring (build HeadlessConfig for resume), review state tracking in WorkIndex.

**Mention routing in channels**: MentionRouted event exists but chat monitor not wired. Need to detect @mentions in `channel.post` content and emit `NudgeAgent` commands targeting the mentioned agent. @lead should route to the channel lead, @ops to ops channel.

**Worktree management**: Workers need isolated git worktrees. CreateWorktree/RemoveWorktree commands exist but executor doesn't handle them. Reuse existing `src/worktree.rs`.

**Session resume on daemon restart**: `resume_on_startup` flag not ported. Agents die when daemon restarts and don't come back.

**Project lead vs channel lead**: The default channel needs a `midtown-project-lead` agent (broad/shallow), while topic channels get `midtown-channel-lead` agents. Currently all leads use `midtown-channel-lead`.

#### Tier 2: Required for Web UI

**Web API (Axum HTTP + WebSocket)**: HTTP routes proxying to RPC dispatch. The web UI needs this to talk to v2. Reuse route structure from `src/web.rs`.

**WebSocket event broadcast**: DomainEvents broadcast to connected WebSocket clients for real-time UI updates.

**GitHub webhooks**: Real-time PR events instead of 45s polling. Reuse `src/webhook.rs` for HMAC verification and event parsing. Wire webhook receiver into the event loop.

#### Tier 3: Quality of Life

**Worker communication**: Workers asking questions of lead/fork. DM channels (`dm-<name>`) with auto-creation. v1 uses `coworker.asking` RPC.

**Creative worker names + icons**: Workers currently named `task-1`. Should use creative naming from word list and Lucide icon assignment for UI display. Add `icon` and `color` to SpawnConfig.

**Auth profile pooling**: Multi-account support for rate limit rotation.

**Reminders**: Cron-based and event-based triggers.

**Garbage collection**: Periodic cleanup of long-dead agent records and stale worktrees.

**Push notifications**: VAPID web push for mobile PWA alerts.

**Lead/fork escalation to user**: @user mentions triggering push notifications or special UI treatment.
