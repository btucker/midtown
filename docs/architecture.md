> Back to [README](../README.md)

# Architecture

## Principles

### Webhooks Are Primary, Polling Adapts

Webhooks handle real-time GitHub events. Polling runs at a relaxed cadence (~2 min) as a backstop for missed deliveries and time-based stuck detection. When webhooks are degraded, polling increases cadence to compensate. Polling should never duplicate a decision that a webhook already triggered.

| Concern | Primary owner | Notes |
|---|---|---|
| PR needs review → spawn reviewer | Webhook | Polling reconciles if missed |
| CI failure → notify owner | Webhook | Polling detects time-based stuck conditions |
| Review comment → nudge owner | Webhook | Polling reconciles if missed |
| Merge conflict → nudge owner | Polling | GitHub doesn't webhook this reliably |
| Approved PR → nudge author | Polling | Author-driven merge decisions |
| Stuck detection | Polling | Inherently time-based |

### Three Communication Paths, Distinct Purposes

- **Initial prompt** — "Here's your mission." One-shot context at spawn time.
- **Channel** — "Here's what's happening." Ambient team awareness, async.
- **Nudge** (headed-intercom delivery for Lead, JSON streaming for coworkers) — "Pay attention now." Synchronous interrupt for session recovery, urgent PR feedback, task assignment to active coworkers.

Don't nudge for information that can wait for the next channel read.

### Decision Functions Are Pure

Functions in `rules.rs` take immutable data and return decisions. No mutation, no I/O, no async. Phase transitions are returned as data, applied by the caller. If a decision depends on a side effect (spawn success, API call), split into two decisions with an effect in between. The `evaluate_tick()` → `Vec<Effect>` → `execute_effects()` pipeline is the canonical path.

This constraint applies to **all functions called from `evaluate_tick()`**, not just those in `rules.rs`. The target architecture has decision-phase functions in domain modules (`pr.rs`, `dispatch.rs`, `health.rs`) also being pure — returning `Vec<Effect>` without performing I/O. Currently, the codebase is migrating toward this pattern: some functions like `collect_merged_pr_cleanup_effects()` in `pr.rs` follow it, while others still use `.await` and `.lock()`. When adding or modifying decision logic, prefer the pure pattern: no `.await`, no `state.persistent_state.lock()`, no `session_manager.is_alive()`, no direct state queries. If data is needed for a decision, add it to `WorldSnapshot` during `collect_world_snapshot()` so it's available as immutable input.

### Daemon Is the Single Authority for State

The daemon owns all coordination state. Coworkers report workflow state via RPC (`midtown` CLI). Pane scraping is a safety net for health checks (stuck, zombie, crash) — not the primary source of workflow information. If RPC and pane scraping disagree, pane scraping wins for health decisions.

### The Channel Is for Communication, Not State

State flows through RPC to the daemon. The channel records events and conversations for awareness. No system should read the channel to determine current state.

### Clear Ownership Between Webhooks and Polling

Each concern has a primary owner. The non-owner path only acts as reconciliation when the primary failed. Enforce via explicit tracking ("webhook handled PR #42"), not passive deduplication (cooldowns).

### Daemon Module Is a Thin Orchestrator

`mod.rs` is the event loop wiring. Domain logic lives in domain modules (`pr.rs`, `health.rs`, `dispatch.rs`, `chat.rs`, `rpc.rs`).

### Names Reflect Actual Responsibility

`SessionMonitorTick` (coworker health), `TaskDispatchTick` (work assignment). Name components for what they do, not their historical origin.

## Key Patterns

**Effect-based side effects**: Never perform I/O in decision functions. Return `Effect` variants from `rules.rs`, execute them in `effects.rs`. This keeps the core logic pure and testable.

**Temp-file pattern for shell arguments**: When passing long text to the `claude` CLI (system prompts, initial prompts), write to a temp file and use `$(cat file)` in the command string. This avoids shell quoting issues. See prompt writing in `launch.rs`.

**Hybrid process model**: The Lead runs in a terminal pane managed by a launcher; coworkers run as headless Claude Code sessions. Lead nudges flow through headed intercom queues; coworker nudges use JSON streaming via `SessionManager`.

---

# How It Works

## Daemon

The daemon is the central coordinator. It runs an event-driven state machine that collects an immutable snapshot of the world each tick, makes pure decisions about what should happen, and then executes the resulting effects. This strict separation between decision logic and side effects keeps the core testable.

The daemon handles:
- Session lifecycle (spawning, health checks, stuck detection, shutdown via session-centric model)
- Task assignment and dispatch
- GitHub webhook processing (PR events, CI status, reviews)
- PR polling for merge conflicts and stuck conditions
- @mention routing between team members
- Topic channel message routing to channel leads
- Headed wrapper intercom RPC endpoints (`headed.register/poll/ack/...`)

## Daemon Startup Sequence

When the daemon starts, it executes a careful cleanup and recovery sequence in `src/daemon/startup.rs` before accepting any events:

1. **PID lock acquisition** — The daemon opens `~/.midtown/projects/<repo>/daemon.pid` and acquires an exclusive file lock. Since a new daemon acquires the lock atomically, any PID recorded in the file belongs to a stale process that lost the lock without exiting.

2. **Stale daemon cleanup** — If the PID file contains a PID from a prior daemon, `kill_stale_daemon()` verifies the process is still running and belongs to *this project's* midtown daemon (by checking that the process cmdline contains "midtown" and the project workdir). If confirmed, it sends SIGTERM and waits up to 3 seconds before escalating to SIGKILL. This handles the case where the old daemon lost its lock (e.g., after a binary rebuild) but didn't exit.

3. **Session PID collection** — Before running the zombie scanner, `recoverable_session_pids()` reads persisted headless sessions from `~/.midtown/projects/<repo>/persistent_state.json` and collects the PIDs of sessions marked `resume_on_startup`. These PIDs are excluded from the zombie scanner — they are intentionally detached and will die naturally from broken pipes when their previous daemon's stdin/stdout closes.

4. **Zombie scanner** — `kill_zombie_claude_processes()` uses `pgrep` to find Claude headless processes matching the midtown settings pattern, then kills:
   - Processes with PPID=1 (truly orphaned — parent exited)
   - Processes whose parent is a stale midtown daemon (PPID is a non-current midtown process)
   - Excludes processes in the session-survival exclusion list from step 3
   - Excludes tmux-managed processes
   - Verifies each candidate PID still belongs to a claude process before killing (guards against PID reuse between `pgrep` and the kill call)
   - Uses SIGTERM → 2s poll loop → SIGKILL (mirrors `kill_stale_daemon`'s responsive wait strategy)

5. **Task assignment restore** — `restore_task_assignments_from_disk()` repopulates the in-memory task→coworker map from disk before any dispatch ticks fire, preventing duplicate coworker spawns.

6. **Session recovery** — `recover_headless_sessions()` generates `ResumeCoworker` effects for each resumable session. The old process is NOT killed here — it dies naturally from the broken pipe when its previous daemon's handles are closed. A fresh `claude --resume <session_id>` process is spawned to continue the session.

## Coworkers

Each coworker runs as:

- A headless Claude Code process (`claude -p --output-format stream-json`) managed by the daemon's `SessionManager`
- In an isolated git worktree (no merge conflicts during development)
- With `--add-dir` worktrees for additional repos in multi-repo projects
- Nudges are delivered via stdin JSON, and health is monitored via stdout stream events

### Session-Centric Model

The daemon uses a **session-centric model** where Claude Code sessions (keyed by session ID) are the primary coordination entity. Names are ephemeral labels drawn from an LRU pool.

**NamePool** (`src/name_pool.rs`): Manhattan avenue names (lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon) are managed in an LRU queue. When a session spawns, it allocates a name from the front of the queue. When it shuts down, the name returns to the back. Preferred name hints allow a resumed session to get its previous name when available, preserving branch and worktree continuity.

**SessionRecord** (`src/daemon/state.rs`): Each session is tracked by a `SessionRecord` containing session ID, task ID, current and preferred names, worktree path, branch, PR number, and running state. Records persist across daemon restarts in `persistent_state.json`.

**Dispatch** (`src/daemon/dispatch.rs`): `dispatch_via_sessions()` is a pure function that examines in-progress tasks with session records. For stopped sessions, it emits `SpawnSession` effects with `resume=true` and the session's preferred name. This replaces the legacy orphan-recovery pattern with a unified session-aware dispatch path.

**In-memory reverse maps** on `DaemonState`:
- `name_to_session` / `session_to_name` — bidirectional name↔session lookup
- `task_to_session` — task→session mapping for dispatch decisions

On daemon startup, the `NamePool` is restored from persisted session records: names with active sessions are marked allocated, the rest are available in LRU order.

## Channel Leads

Channel leads are headless Claude Code sessions attached to individual topic channels. Where coworkers are temporary implementers that come and go with tasks, channel leads are long-lived domain experts that accumulate context across conversations.

**Role:** A channel lead brainstorms, maintains living design documents, answers domain questions, and tracks awareness of active tasks and PRs in its channel. It does not write code, open PRs, or create tasks. When implementation work is needed, it escalates to @lead.

**Message routing:** When a user posts to a topic channel (any non-main channel), `handle_channel_post` in `src/daemon/mod.rs` nudges the channel lead for that channel via `SessionManager::send_message`. If no channel lead session is alive for that channel, the message is silently skipped — it remains in the channel log and is available when the channel lead next starts up. Main channel behavior is unchanged. Note: `route_mentions()` is intentionally disabled for topic channels — user `@coworker` and `@all` mentions in topic channels are silently dropped; only the channel lead nudge path is active.

**System prompt:** Channel leads use the `agents/channel-lead.md` template, instantiated with `{channel_name}` and `{domain_context}` via `channel_lead_system_prompt()` in `src/agents.rs`.

**Coworker guidance:** Coworkers are instructed to `@{channel-name}` for domain questions (e.g., architecture, design decisions) and to reserve `@lead` for coordination, task, and priority questions.

## Channel Storage Layout

Each channel is stored as a directory under `~/.midtown/projects/<repo>/channels/`:

```
channels/
  midtown/                          # main project channel
    history/
      current.jsonl                 # active message file
      2026-02-18.jsonl              # rotated daily archive
    notes/                          # channel lead domain knowledge (markdown)
    cursors/
      <session_id>.json             # per-session read position (keyed by session ID)
  pr-42/                            # topic channel
    history/current.jsonl
    notes/
    cursors/
  old-feature.archived/             # archived channel (.archived suffix)
    history/current.jsonl
    notes/
    cursors/
```

**Auto-migration:** On first `Channel::new()` per process, `auto_migrate_channels()` converts legacy layouts:
- `channel.jsonl` → `channels/midtown/history/current.jsonl` (V0→V3)
- `channels/<name>.jsonl` → `channels/<name>/history/current.jsonl` (V2→V3)
- `cursors/<agent>.json` → deleted (cursors are now session-scoped and ephemeral)

Migration runs once per `base_dir` per process (via `OnceLock`) and is idempotent.

**Channel RPC methods** (handled by `src/daemon/rpc_channel.rs`):
- `channel.post` — Append a message to a channel; handles `/me` actions, @mention routing, review note deduplication
- `channel.read` — Read messages from a channel (supports `all`, `last`, `since`, and per-channel filtering)
- `channel.create` — Create a new channel directory; idempotent (no-op if channel already exists)
- `channel.archive` — Rename `channels/<name>/` to `channels/<name>.archived/`; returns an error if the channel does not exist or if archiving 'midtown'
- `channel.list` — Return all channels, optionally including archived ones

## Channel Sync

Coworkers stay synchronized via a Claude Code Stop hook. When Claude pauses, the hook reads new channel messages and checks for unclaimed tasks. This means coworkers automatically receive updates at natural pause points.

## Nudge System

Nudge decisions are made in `src/rules.rs` (`decide_interrupt_nudges`, `decide_prompt_nudges`) using `CooldownTracker` for per-coworker cooldowns and `CoworkerPhase` for deduplication (Idle → Prompted → Interrupted). Delivery is via `Effect::NudgeCoworker` / `Effect::NudgeLead` in `src/daemon/effects.rs`:

- **Lead nudges**: Delivered through headed intercom queues (`headed.register/poll/ack`) with tmux fallback
- **Coworker nudges**: JSON streaming via `SessionManager` for headless sessions

## Mailbox Messaging

In addition to the shared channel, the daemon can deliver targeted messages to individual coworkers via the Claude Code agent teams mailbox protocol. Messages are written as JSON to `~/.claude/teams/{team-name}/inboxes/{agent-name}.json` using atomic file operations with mkdir-based locking for safe concurrent access.

## Worktree Lifecycle

When a coworker is called in, midtown creates a detached git worktree at the current HEAD. The coworker creates a feature branch and works independently. When the coworker shuts down, worktrees with no commits and no uncommitted changes are automatically cleaned up along with their branches. Worktrees with work in progress are preserved.

## GitHub Integration

The daemon receives real-time GitHub events via webhooks (PR creation, reviews, check runs) verified with HMAC-SHA256 signatures. PR polling runs as a backstop for missed webhook deliveries and handles time-based concerns like merge conflict detection and stuck PR identification.

## Webhook Ports

Each project daemon runs its own webhook server for GitHub integration. Port 47022 is reserved for the shared multi-project webserver. Per-project daemons auto-assign ports starting at 47023, persisting the assignment in the project's `config.toml` for stability across restarts.

## Thread Storage

Thread replies are stored in the same JSONL channel file as top-level messages, tagged with a `thread_parent_id` field referencing the parent message's ID. There is no separate index — threads are filtered in memory at read time by comparing `thread_parent_id` against the queried parent ID. Top-level messages have `thread_parent_id` omitted (serialized with `skip_serializing_if = "Option::is_none"` for backward compatibility with existing channel logs).

The `/api/channels/history` endpoint accepts an optional `thread_parent_id` query parameter:
- Absent: returns only top-level messages (where `thread_parent_id` is `None`), with `reply_count` and `last_reply` metadata when replies exist
- Present: returns only thread replies matching the given parent ID

When a thread reply is posted via `midtown channel post --thread <id>`, the daemon automatically nudges all thread participants (the original message author and all authors of existing replies in that thread), ensuring no reply goes unnoticed.

## Chat TUI

The `midtown chat` command opens a split-panel interface with:

**Layout**:
- **Board panel** (left 40%): Channel swimlanes showing in-progress (●) and pending (○) tasks per channel
- **Chat panel** (right 60%): Real-time message display with mermaid diagram rendering
- **Input bar** (bottom): Text input for posting messages (Tab to focus, Enter to send)

**Features**:
- Real-time channel message display
- Mermaid diagram detection and rendering (via `selkie-rs` with content-hash caching)
- Inline ASCII art for flowchart diagrams (press number keys to open SVG in browser)
- **Type-anywhere UX**: Character keys auto-focus the input bar (like Slack/Discord)
- Tab-based focus navigation (Board → Chat → InputBar)
- Arrow keys, PageUp/PageDown, Home/End for scrolling
- Mouse support for scrolling and navigation
- Clickable hyperlinks via OSC 8 escape sequences
- Real-time token usage and cost tracking

**Data polling**:
- **Coworker state** (2s): `coworkers.status` RPC — live in-memory data, no GraphQL.
- **PR data** (30s): `prs.status` RPC — GitHub GraphQL, daemon-cached for 60s.
- **Repo status** (60s): Direct `gh` CLI calls for commit/CI/release info.

The split-poll architecture ensures coworker phase changes appear in real-time (2s) while expensive PR data is fetched at a rate that stays within the daemon's 60s cache TTL.

## Web UI

The web interface is a Svelte 5 + Vite SPA served on port 47022:

- Installable as a PWA for mobile use
- Real-time updates via WebSocket
- Kanban board for task visualization
- Multi-channel support with split-panel layout (channel list sidebar + message pane)
- Channel list with task counts (in progress, pending) and CI status badges
- Channel header displays channel-specific stats (PR count, in-progress tasks, pending tasks) that update when switching channels
- Create new channels directly from the sidebar (+ button) with inline validation
- Clickable channel (`#name`), task (`!N`), and PR (`#N`) references in messages
- Insight cross-post highlighting with source channel attribution
- Mermaid diagram rendering in chat messages
- Image and document paste support (clipboard → inline preview → upload to lead)
- Coworker status monitoring
- Auth profile switching
- Push notifications (W3C Push API with VAPID)
- Responsive layout with three breakpoints:
  - **Mobile (≤768px)**: Tab navigation, hamburger menu with slide-out sidebar, modal popups for task/PR details
  - **Tablet (769–1024px)**: Permanent sidebar replaces tab navigation, two-column grid layout
  - **Desktop (≥1025px)**: Three-column Slack-inspired layout with sidebar, main channel, and toggleable detail panel for tasks, PRs, and coworker info
- Clickable `@coworker` mentions in messages open coworker detail panel on desktop

## Universal Events Pipeline

The `universal_events` module (`src/universal_events/`) provides a provider-agnostic event model for structured agent activity. It captures tool calls and tool results from Claude Code's `stream-json` output, stores them in daemon memory, and broadcasts them to WebSocket clients and TUI as structured data, parallel to the existing text pipeline.

**Data flow:**
```
StreamEvent (NDJSON drain) → extract_tool_events() → Vec<UniversalItem>
    → Effect::BroadcastUniversalItems → DaemonState.recent_tool_items (per-agent ring buffer)
                                      → WebUpdate::UniversalItems → WebSocket clients
                                      → coworkers.status RPC → TUI tool activity display
```

- **Types** (`mod.rs`): `UniversalItem`, `ItemKind`, `ContentPart`, `ItemStatus` — agent-agnostic, extensible to other providers.
- **Claude converter** (`claude.rs`): Pure function `extract_tool_events()` that extracts both `tool_use` content blocks from `StreamEvent::Assistant` events and `tool_result` blocks from `StreamEvent::User` events. Each tool call is emitted with a `semantic_header` (human-readable description of the operation) and each tool result carries success/error status.
- **Integration** (`daemon/stream.rs`): `process_universal_events()` accepts the `channel_lead_sessions` map and emits `BroadcastUniversalItems` effects for the main lead (channel=None) and for each active channel lead (channel=Some(channel_name)). Coworker tool calls are never broadcast.
- **Broadcast**: The `BroadcastUniversalItems` effect sends `WebUpdate::UniversalItems` to all connected WebSocket clients and updates `DaemonState.recent_tool_items` (a `RwLock<HashMap<String, Vec<UniversalItem>>>`, capped at `MAX_TOOL_ITEMS_PER_AGENT=20` items per agent). Agent name and optional channel are carried at the envelope level (`UniversalItemsData`). The web UI stores items keyed by channel name (`'midtown'` for the main lead, or the topic channel name for channel leads) so each channel view shows only the relevant tool calls.
- **TUI rendering**: The TUI polls `coworkers.status` (at 2s intervals) which calls `collect_tool_activity()` to serialize `recent_tool_items`. The TUI renders a compact activity strip at the bottom of the chat pane showing the most recent tool calls per active agent, using `semantic_header` for tool call labels and "✓ ok" / "✗ error" for tool results.
- **Lifecycle**: Tool activity for a coworker is cleared from `recent_tool_items` when the coworker shuts down (in `shutdown_coworker_impl()`), preventing ghost activity from persisting when the avenue name is reused.

## Headed Intercom RPC

Headed wrappers are adapter-neutral shims around interactive agent processes.
Each wrapper registers a session lease with the daemon and consumes queued
messages through a poll+ack contract.

**Endpoints:**
- `headed.register` — Claim or refresh an adapter lease for a session (e.g. `lead`).
- `headed.poll` — Read queued messages after a message ID.
- `headed.ack` — Acknowledge delivery up to a message ID (advances queue head).
- `headed.heartbeat` / `headed.unregister` — Maintain or release lease ownership.

**DaemonState fields for intercom support:**
- `headed_sessions: Mutex<HashMap<String, HeadedSessionState>>` — Per-session queue + lease.
- `attached_coworkers: Mutex<HashMap<String, DateTime<Utc>>>` — Tracks interactive attach/detach state for headless coworkers. Keys are coworker names; values are the attach timestamp. Entries are added on `midtown session attach`, removed on `midtown session detach` or via `Effect::AutoDetachCoworker` (auto-detach after `ATTACH_TIMEOUT` = 10 min, to recover from crash/disconnect without detach).

## Coworker Questions (AskUserQuestion)

Coworkers can ask the Lead questions via the Claude Code `AskUserQuestion` tool. The flow:

1. Coworker calls `AskUserQuestion` → Claude Code CLI runs `midtown coworker asking` → daemon RPC `coworker.asking`
2. Daemon stores the question in `DaemonState.pending_questions` (ephemeral, one active question per coworker), posts to channel, nudges the Lead
3. Daemon broadcasts `WebUpdate::CoworkerQuestion` to WebSocket clients (Web UI, TUI)
4. TUI polls `coworker.questions` RPC on each kanban refresh; Web UI hydrates from `/api/questions` on connect
5. Lead answers via TUI input or Web UI → `coworker.nudge` RPC → daemon delivers answer and clears the pending question

**DaemonState fields:**
- `pending_questions: Mutex<Vec<PendingQuestion>>` — In-memory store of unanswered questions. Cleared on nudge delivery, coworker cleanup, or daemon restart.
- `pending_question_id_counter: AtomicU64` — Monotonically increasing ID for question deduplication.

**RPC methods:**
- `coworker.asking` — Store a pending question and notify the Lead
- `coworker.questions` — Return all pending questions (used by TUI polling and `/api/questions` endpoint)

## Reminders

The Lead can set reminders that trigger on specific conditions:

```bash
# Remind me when all tasks are done and PRs merged
midtown lead remind all-work-merged "Time to deploy!"

# List active reminders
midtown lead remind list

# Cancel a reminder
midtown lead remind cancel <id>
```

Reminders are stored in `~/.midtown/projects/<repo>/reminders.json` and evaluated by the daemon each tick.
