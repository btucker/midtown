# Midtown Daemon V2 — Functional Specification

| Field | Value |
|-------|-------|
| **Status** | In progress |
| **Authors** | @btucker |
| **Last updated** | 2026-03-29 |
| **Tracking** | [v2 design spec](superpowers/specs/2026-03-28-daemon-v2-design.md), [v2 architecture](v2-architecture.md) |

## 1. Overview

Midtown's daemon coordinates autonomous AI coding agents across a repository. V2 is an event-sourced rewrite that replaces V1's mutable state model while preserving CLI and web UI compatibility.

### 1.1 Goals

- **Reliable state**: Event-sourced persistence with crash-safe recovery. No more state corruption from partial writes.
- **Simpler model**: One projection per domain (agents, work, channels) replaces 50+ ad-hoc tick fields.
- **Resume-on-nudge**: Stopped agents are resumed when addressed, instead of silently dropping messages.
- **Multi-channel leads**: Every active channel gets its own lead agent, not just the default.
- **Reviewer escalation**: Failed reviewers escalate to humans instead of restarting forever.

### 1.2 Non-Goals

- New user-facing features beyond parity with V1.
- Changes to the web UI frontend (it talks to the same API).
- Changes to agent definitions or the Claude Code CLI interface.

## 2. User-Facing Behavior

### 2.1 Starting the Daemon

```bash
MIDTOWN_DAEMON_V2=1 midtown start
```

Same socket path as V1. The CLI and web UI work without changes.

### 2.2 Agents

An **agent** is a managed Claude Code process. Three kinds:

| Kind | Bound to | Named | Example |
|------|----------|-------|---------|
| **Lead** | Channel | After the channel | `main`, `backend` |
| **Fork** | Thread | `fork-{thread_id_prefix}` | `fork-abc12345` |
| **Worker** | Task | Creative name | `ghost-town`, `swift-river` |

Agents run in isolated git worktrees. Workers get task-specific branches; leads and forks share a lead worktree.

### 2.3 Task Lifecycle

```
Pending  ──dispatch──▶  InProgress  ──PR merges──▶  Completed
   ▲                        │
   └──agent dies────────────┘  (reset to pending, re-dispatched)
```

- Tasks can declare `blocked_by` dependencies. Only unblocked pending tasks are dispatched.
- Up to 3 tasks run concurrently (configurable).
- When a worker dies, the task resets to pending and a new worker is spawned.
- When two workers claim the same task, the older one is stopped.
- Idle workers (no task for 5 minutes) are shut down.

### 2.4 Message Routing

Every message posted to a channel produces nudges. The rules, in priority order:

1. **Thread reply with a bound agent** → Nudge that agent.
2. **Thread reply without a bound agent** → Nudge the channel lead.
3. **Top-level message** → Nudge the channel lead.
4. **@mention** → Nudge the named agent.
5. **@all** → Nudge every agent in the channel.
6. **@lead / @channel-name** → Nudge the channel lead.
7. **!N task reference** → Nudge the agent assigned to task N.

**Invariants:**
- Self-nudges are always suppressed (sender == agent name).
- Each agent is nudged at most once per message (deduplication).
- Running state is irrelevant — stopped agents are resumed before the message is delivered.
- Routing is determined by bindings (thread, channel, name, task), not agent type.

### 2.5 PR Integration

The daemon polls GitHub every 45 seconds for PR state changes.

| Event | Response |
|-------|----------|
| PR opened | Link to task if title contains `[Midtown !N]` |
| PR needs review | Spawn a reviewer agent |
| Reviewer dies | Respawn (up to 3 attempts), then escalate to ops channel |
| PR approved + CI green | Suspend the author agent (it's waiting for merge) |
| PR merged | Complete the linked task; clean up worktree; nudge other workers to rebase |
| PR closed without merge | No action (task stays in progress) |

Rebase nudges use a 1-hour per-agent cooldown to prevent spam.

### 2.6 Channel Leads

Every non-archived channel gets a lead agent, spawned automatically. The default channel's lead uses `midtown-project-lead`; topic channels use `midtown-channel-lead`. Leads receive every message in their channel.

Channel settings:
- **`lead_driven`** — When true, automatic task dispatch is skipped for this channel. The lead manages work manually.
- **`directory`** — Subdirectory of the repo (e.g., `packages/auth`). The lead loads AGENTS.md/CLAUDE.md from this directory.

### 2.7 Session Persistence

- Agent sessions survive daemon restarts. On startup, the daemon checks PIDs, marks dead agents, and resumes those with session IDs.
- Thread bindings (fork → thread) persist through agent stops. A stopped fork is resumed when someone posts to its thread.
- Agent records are garbage-collected 24 hours after stopping. Leads are never GC'd.

### 2.8 Web UI

REST API + WebSocket on the configured port. The shared webserver (port 47022) proxies to it.

| Endpoint | Purpose |
|----------|---------|
| `GET /api/status` | Agent/task/PR counts for dashboard |
| `GET /api/channels` | List all channels |
| `GET /api/channels/history` | Message history with thread filtering |
| `GET /api/search?q=...` | Full-text search across channels |
| `POST /api/channels/create` | Create a channel |
| `GET /api/ws` | WebSocket: real-time domain event stream |

## 3. Behavioral Changes from V1

| Behavior | V1 | V2 |
|----------|----|----|
| Dead forks | Stay dead. Thread replies fall through to lead. | Resumed when someone posts to their thread. |
| @mention stopped agent | Silently dropped. | Agent is resumed, then receives the message. |
| Channel leads | Only default channel gets auto-spawned lead. | All active channels get leads. |
| Reviewer failures | Restart indefinitely. | Escalate to ops after 3 failures. |
| Rebase nudging | Not implemented. | Workers nudged to rebase after PR merges. |
| State persistence | Mutable JSON blob, prone to corruption. | Event-sourced: append-only log + snapshots. |

## 4. Configuration

V2 reads the same `config.toml` as V1.

```toml
[daemon]
webhook_port = 6969          # HTTP port for webhooks + web UI
max_in_progress_tasks = 8    # concurrent task limit
```

| Environment Variable | Purpose |
|---------------------|---------|
| `MIDTOWN_DAEMON_V2=1` | Launch V2 instead of V1 |
| `MIDTOWN_WEBHOOK_PORT=0` | Disable webhook server (testing) |
| `MIDTOWN_CHAT_MONITOR=0` | Disable chat monitor (testing) |

## 5. Implementation Status

### 5.1 Complete

- Event store with JSONL append, snapshots, crash-safe recovery
- Agent lifecycle: spawn, stop, resume, nudge, GC
- Task dispatch with blocking dependencies and duplicate detection
- PR polling, reviewer spawning with escalation, rebase nudging
- Multi-channel lead management
- Binding-based message routing with resume-on-nudge
- RPC with V1 compatibility aliases
- Web API with WebSocket event broadcast
- Worktree management (create, reuse, cleanup on task completion)

### 5.2 Not Yet Ported

**Critical:**
- Webhook forwarder watchdog (`gh webhook forward` process management)
- Background chat monitor (tail loop on channel JSONL)
- GitHub API rate limit monitoring
- Auth profile pooling (multi-account rotation)

**Important:**
- Reminder system (cron + event-based triggers)
- Workflow system (assignment, state machine)
- Task prompt / handoff between agents
- Session attach/detach (interactive takeover)
- CI issue detection (stale checks, auto-rerun)

**Nice to have:**
- Channel rename/merge
- Oneshot execute
- Daemon exec-restart / draining mode
- Push notifications
- RPC response caching

## 6. Revision History

| Date | Change |
|------|--------|
| 2026-03-29 | Initial spec. Covers agent lifecycle, task dispatch, PR integration, message routing, multi-channel leads, reviewer escalation, resume-on-nudge. |
