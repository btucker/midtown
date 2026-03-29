# Daemon V2 — Living Spec

This is the user-facing specification for the v2 daemon. It describes what works, what's planned, and what's different from v1.

**Last updated:** 2026-03-29

## How to Use

```bash
# Start the v2 daemon
MIDTOWN_DAEMON_V2=1 midtown start

# Everything else works the same — same CLI, same web UI
midtown status
midtown channel post "hello"
midtown task create --subject "Fix the bug"
```

The v2 daemon uses the same Unix socket path as v1, so the CLI and web UI work transparently.

## What Works

### Agent Management
- Spawn workers, leads, and forks via RPC or web UI
- Agents run in isolated git worktrees
- Creative agent names, Lucide icons, CSS colors
- DM channels auto-created for workers
- Stop, nudge, and list agents

### Task Dispatch
- Create tasks with subjects, channels, and blocking dependencies
- Automatic dispatch: pending tasks get workers spawned (up to 3 concurrent)
- Dead worker detection: tasks reset to pending when their agent dies
- Duplicate detection: if two agents claim the same task, the older one is stopped
- Idle worker shutdown after 5 minutes without a task

### PR Integration
- Polls GitHub for open and merged PRs (every 45s)
- Links PRs to tasks automatically
- Spawns reviewer agents for PRs needing review
- Reviewer escalation: after 3 failed review attempts, posts to ops channel
- Completes tasks when their PR merges
- Suspends author agents while PR is in review
- Nudges workers with open PRs to rebase after another PR merges (1hr cooldown)

### Channels
- Multi-channel support with per-channel leads
- Channel settings: `lead_driven` mode, `directory` for AGENTS.md loading
- Archive/unarchive channels
- Full-text search across all channels
- Thread support with thread-bound fork sessions

### Message Routing
- **Every message nudges the channel lead** (so it stays aware)
- **Thread replies nudge the thread-bound agent** (fork or dedicated session)
- **@mentions nudge the named agent** — even if stopped (the daemon resumes it)
- **@all broadcasts to every agent** in the channel
- **@lead / @channel-name** routes to the channel lead
- **!N task references** nudge the agent working on that task
- Agents that have stopped are **automatically resumed** when nudged

### Session Lifecycle
- Sessions persist through daemon restarts (resume via session_id)
- Thread bindings persist through agent stops (fork resumes on thread activity)
- Startup reconciliation: detects dead processes, schedules resumes
- Garbage collection removes stopped agent records after 24 hours

### Web UI
- Real-time updates via WebSocket
- REST API for channels, status, search, auth profiles
- Channel history with thread filtering
- File upload support

## What's Different from V1

| Behavior | V1 | V2 |
|----------|----|----|
| Dead forks | Stay dead. Thread replies fall through to lead. | Resumed automatically when someone posts to their thread. |
| @mention stopped agent | Silently dropped. | Agent is resumed, then receives the message. |
| Channel leads | Only default channel gets auto-spawned lead. | All active channels get leads. |
| Reviewer failures | Restart indefinitely. | Escalate to ops after 3 failures. |
| Rebase nudging | Not implemented. | Workers nudged to rebase after PR merges (1hr cooldown). |
| State model | Mutable JSON blob (`persistent_state.json`). | Event-sourced: append-only log + snapshots. |
| Cooldowns | 10 different mechanisms. | One unified `CooldownTracker`. |

## What's Not Ported Yet

These v1 features are not yet available in v2. See [the gap analysis](../docs/superpowers/specs/2026-03-28-daemon-v2-design.md#implementation-status) for details.

### Tier 1 — Critical
- **Webhook forwarder watchdog** — v2 accepts webhook events but doesn't manage the `gh webhook forward` process
- **Chat monitor** — no background tail loop on channel JSONL (mention routing only on explicit `channel.post` calls)
- **Rate limit monitoring** — no GitHub API quota tracking
- **Auth profile pooling** — no multi-account rotation

### Tier 2 — Important
- **Reminder system** — cron and event-based triggers (stubbed)
- **Workflow system** — workflow assignment and state machine (stubbed)
- **Task prompt / handoff** — deliver prompts to tasks, transfer between agents (stubbed)
- **Session attach/detach** — interactive session takeover (stubbed)
- **CI issue detection** — stale check detection, auto-rerun

### Tier 3 — Nice to Have
- Channel rename/merge
- Oneshot execute
- Daemon exec-restart / draining mode
- Push notifications (VAPID web push)
- RPC response caching

## Configuration

V2 reads the same `config.toml` as v1. Key settings:

```toml
[daemon]
webhook_port = 6969          # HTTP port for webhooks + web UI
max_in_progress_tasks = 8    # task concurrency limit (v2 default: 3)
```

Environment variables:
- `MIDTOWN_DAEMON_V2=1` — launch v2 instead of v1
- `MIDTOWN_WEBHOOK_PORT=0` — disable webhook server (testing)
- `MIDTOWN_CHAT_MONITOR=0` — disable chat monitor (testing)
