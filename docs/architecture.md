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

**Hybrid process model**: The Project Lead runs in a terminal pane managed by a launcher; coworkers run as headless Claude Code sessions. Project Lead nudges flow through headed intercom queues; coworker nudges use JSON streaming via `SessionManager`.

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

6. **Session recovery** — `recover_from_session_records()` generates `ResumeCoworker` effects for each resumable session (those with `is_running=true` and `resume_on_startup=true`). Channel leads are skipped here and recovered separately in step 8. The old process is NOT killed here — it dies naturally from the broken pipe when its previous daemon's handles are closed. A fresh `claude --resume <session_id>` process is spawned to continue the session.

7. **Stale flag cleanup** — `clear_stale_running_sessions()` clears the `is_running` flag for any session not included in the recovered set. This covers sessions skipped by `recover_from_session_records` for any reason (non-resumable, reviewer without a PR number, or dropped by name deduplication), as well as channel-lead sessions whose channel was archived between daemon runs. Active channel-lead sessions (whose channel is still non-archived) are preserved for the separate channel lead recovery path.

8. **Channel lead recovery** — `recover_channel_lead_sessions()` iterates active (non-archived) topic channels and emits `SpawnCoworker` effects to resume or fresh-start each channel lead session. The main channel names `"midtown"` and `"main"` are always excluded from this step — they belong to the Project Lead, not a channel lead. This guards against accidentally recreated channel directories (e.g., from tests or legacy TUI sessions).

## Coworkers

Each coworker runs as:

- A headless Claude Code process (`claude -p --output-format stream-json`) managed by the daemon's `SessionManager`
- In an isolated git worktree (no merge conflicts during development)
- With `--add-dir` worktrees for additional repos in multi-repo projects
- Nudges are delivered via stdin JSON, and health is monitored via stdout stream events

### Session-Centric Model

The daemon uses a **session-centric model** where Claude Code sessions (keyed by session ID) are the primary coordination entity. Names are ephemeral labels drawn from an LRU pool.

**NamePool** (`src/name_pool.rs`): Manhattan avenue names (lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon) are managed in an LRU queue. When a session spawns, it allocates a name from the front of the queue. When it shuts down, the name returns to the back. Preferred name hints allow a resumed session to get its previous name when available, preserving branch and worktree continuity. Name allocation and release both clear the agent's mailbox inbox to prevent message bleed between sessions (see Mailbox Messaging).

**SessionRecord** (`src/daemon/state.rs`): Each session is tracked by a `SessionRecord` containing session ID, task ID, current and preferred names, worktree path, branch, PR number, and running state. Records persist across daemon restarts in `persistent_state.json`.

**Dispatch** (`src/daemon/dispatch.rs`): `dispatch_via_sessions()` is a pure function that examines in-progress tasks with session records. For stopped sessions, it emits `SpawnSession` effects with `resume=true` and the session's preferred name. This replaces the legacy orphan-recovery pattern with a unified session-aware dispatch path.

**In-memory reverse maps** on `DaemonState`:
- `name_to_session` / `session_to_name` — bidirectional name↔session lookup
- `task_to_session` — task→session mapping for dispatch decisions

On daemon startup, the `NamePool` is restored from persisted session records: names with active sessions are marked allocated, the rest are available in LRU order.

## Prompt Architecture

Prompts are assembled from composable markdown files in `agents/` and loaded at runtime by `src/agents.rs`. The file-based approach allows customization without recompilation: the binary embeds defaults, but `agents/` in the git repo root (or `~/.midtown/agents/`) takes precedence.

**Assembly by agent type:**
- **Project Lead**: `project-lead.md` + `lead.md` + `common.md`
- **Coworker**: `coworker.md` + `common.md`
- **Reviewer**: `coworker.md` + `common.md` + `reviewer.md`
- **Channel lead**: `channel-lead.md` (with optional `ops-channel-lead.md` suffix for the ops channel)

**Template variables:** `{name}` (agent name; project name for Project Lead), `{project_name}` (e.g., `midtown`), `{channel_name}`, `{domain_context}` (channel lead only).

**@mention routing:** Agents use `@{project_name}` (e.g., `@midtown`) to mention the Project Lead — not the literal `@lead`. Both `@lead` and `@{project_name}` are recognized by the daemon's nudge routing in `rpc_channel.rs` and `chat.rs`.

**Task-based @mention routing:** When a lead @mentions a coworker and includes a task ID (`!N`), the daemon's `route_mentions()` in `chat.rs` resolves the actual session to nudge by looking up the task owner from the task system (`crate::tasks::get_in_progress_tasks_with_subjects_for_repo`). If the resolved owner is running, the nudge is routed to them instead of the @mentioned name — this ensures feedback reaches the correct session even if coworker names have been reassigned. Example: `@park !42 here's your review feedback` routes to whoever is working on task 42. Falls back to name-based routing when the task ID is not found or the owner is not running. **Note:** `Coworker.current_task` is a display-only field (always `None` in storage, populated dynamically for API responses); the task system file store is the authoritative source for task ownership.

## Main Lead Session Identity

The main lead session name equals the repo name (e.g. `"midtown"`), not the hardcoded string `"lead"`. This applies everywhere:

- **Spawn**: `LaunchConfig::lead()` sets `name = repo_name.clone()` (`src/launch.rs`)
- **Health**: `ensure_lead_alive()` and `maybe_refresh_lead_session()` compare against `snap.repo_name` (`src/daemon/health.rs`)
- **Dispatch**: coworker-limit checks use `snap.repo_name` (`src/daemon/dispatch.rs`)
- **Effects**: auto-detach suffix check and skip-filter use `state.repo_name` (`src/daemon/effects.rs`)
- **Stop-time key**: `coworker_stop_times` entries for the lead are keyed by `repo_name.to_lowercase()`
- **Attached key**: `attached_coworkers` entries for the lead are keyed by `repo_name` (lowercase)

Code that previously compared `name == "lead"` now compares `name.eq_ignore_ascii_case(&snap.repo_name)` or checks `coworker_type == Some("lead")` (for attach-path role detection).

## Provider Resolution

Each session role resolves its AI provider via `get_execution_provider_for_role()` in `src/config.rs`. The resolution chains are:

- **Lead**: `execution.project_lead_provider` → `execution.lead_provider` → `Claude` (default)
- **Channel Lead**: `execution.channel_lead_provider` → `execution.lead_provider` → `Claude` (default)
- **Coworker**: `execution.coworker_provider` → `Claude` (default)
- **Reviewer**: `execution.reviewer_provider` → `Claude` (default)
- **Architect / HeadlessExecute**: role-specific provider → `execution.specialized_provider` → `Claude` (default)

This means `lead_provider` acts as a shared fallback for both the Project Lead and Channel Leads. Setting `project_lead_provider` overrides only the Project Lead's provider without affecting channel leads, and vice versa for `channel_lead_provider`. The resolved provider is stored in `LaunchConfig.auth_provider` and is also used to derive the default model via `default_model_for_provider_role()`.

## Channel Leads

Channel leads are headless Claude Code sessions attached to individual topic channels. Where coworkers are temporary implementers that come and go with tasks, channel leads are long-lived domain experts that accumulate context across conversations.

**Role:** A channel lead brainstorms, maintains living design documents, answers domain questions, and tracks awareness of active tasks and PRs in its channel. It does not write code, open PRs, or create tasks. When implementation work is needed, it escalates to `@{project_name}`.

**Message routing:** When a user posts to a topic channel (any non-main channel), `handle_channel_post` in `src/daemon/rpc_channel.rs` nudges the channel lead for that channel via `SessionManager::send_message`. If no channel lead session is alive for that channel, the message is silently skipped — it remains in the channel log and is available when the channel lead next starts up. Main channel behavior is unchanged. Note: `route_mentions()` is intentionally disabled for topic channels — user `@coworker` and `@all` mentions in topic channels are silently dropped; only the channel lead nudge path is active.

**System prompt:** Channel leads use the `agents/channel-lead.md` template, instantiated with `{channel_name}`, `{domain_context}`, and `{project_name}` via `channel_lead_system_prompt()` in `src/agents.rs`.

**Coworker guidance:** Coworkers are instructed to `@{channel-name}` (e.g., `@daemon-core`) for domain questions and to reserve `@{project_name}` for coordination, task, and priority questions.

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

- **Project Lead nudges**: Delivered through headed intercom queues (`headed.register/poll/ack`) with tmux fallback
- **Coworker nudges**: JSON streaming via `SessionManager` for headless sessions

## Mailbox Messaging

In addition to the shared channel, the daemon can deliver targeted messages to individual coworkers via the Claude Code agent teams mailbox protocol. Messages are written as JSON to `~/.claude/teams/{team-name}/inboxes/{agent-name}.json` using atomic file operations with mkdir-based locking for safe concurrent access.

**Inbox lifecycle**: Inboxes are cleared at two points to prevent message bleed across sessions: (1) when a name is allocated from the `NamePool` during `SpawnSession` (before the new session starts), and (2) when a session releases its name on shutdown. This ensures a newly-allocated name never inherits stale unread messages from a previous session that held the same name. All inbox operations — writes, reads, and clears — acquire the same mkdir-based lock (`{agent-name}.json.lock`) to prevent races.

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
- **Task detail panel** (replaces chat, 60%): Shown when a task in the board is clicked (Enter/mouse). Displays subject, status, owner, channel, PR link, blocked-by, and description. Dismissed with Esc.
- **Thread panel** (replaces chat, 60%): Shown when a message line is clicked. Mutually exclusive with the task panel.
- **Input bar** (bottom): Text input for posting messages (Tab to focus, Enter to send)

**State fields** (`App` in `app.rs`):
- `open_task_id: Option<String>` — task currently shown in the detail panel; `None` when closed
- `thread_parent_id: Option<String>` — message whose thread is open; `None` when closed
- `focused_pane: FocusedPane` — `Board | Chat | InputBar | Thread`; controls keyboard routing

**Task panel behavior**:
- Click or Enter on a board task → `open_task()` → sets `open_task_id`, clears thread state, resets `focused_pane` to `InputBar` if thread was focused
- Esc → `close_task()` → clears `open_task_id`, resets `focused_pane` to `InputBar`
- Channel switch → `load_channel_messages()` → clears `open_task_id` to prevent stale panel
- Task panel and thread panel are mutually exclusive; opening one closes the other
- Rendered by `ui/task_panel.rs` (`draw_task_panel`); shows from the top so metadata is always visible

**Esc key priority** (in order):
1. Clear pending clipboard image
2. Dismiss channel switcher
3. Dismiss autocomplete
4. Close thread (if `focused_pane == Thread`)
5. Clear InputBar input text (if `focused_pane == InputBar` and input non-empty)
6. Close task panel (if `open_task_id.is_some()`)
7. No-op if `focused_pane == InputBar` with empty input
8. Exit TUI (Board or Chat focus with no active panel)

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

## Reviewer Health and Stuck Detection

Reviewers are headless Claude Code sessions assigned to specific PRs. The daemon monitors them for stuck conditions (alive but unresponsive) and dead conditions (process exited before posting a review).

### Stuck Detection

`check_and_restart_stuck_reviewers()` in `health.rs` calls `decide_stuck_reviewer_restarts()` (pure, in `rules.rs`) each `SessionMonitorTick`. A reviewer is considered stuck if it is alive but has emitted no stream events for the stuck threshold duration. The threshold varies per PR:

- **Standard threshold** (`REVIEWER_STUCK_DURATION` = 300s): Used when the reviewer has not posted a placeholder comment.
- **Shorter threshold** (`REVIEWER_PLACEHOLDER_STUCK_DURATION` = 120s): Used when the reviewer has posted a "Review in progress" placeholder comment. Since the placeholder proves the reviewer started the review, a shorter timeout applies to recover faster.

After `MAX_REVIEWER_RESTARTS` attempts per PR, an escalation warning is posted to the ops channel and the lead is nudged. The escalation threshold also uses the per-PR effective duration (shorter for placeholder PRs).

### Dead Reviewer Detection

`check_and_restart_dead_reviewers()` detects reviewers whose process has exited (is_alive = false) without posting a review. This catches natural exits (max turns, context window full) before the review is complete. Dead reviewers are respawned up to `MAX_REVIEWER_RESTARTS` times.

### Placeholder Comment Handling

When a reviewer is restarted (stuck or dead) and had previously posted a "Review in progress" placeholder, the daemon patches the comment via `Effect::UpdatePrComment` to indicate the reviewer timed out and a replacement was assigned. This keeps the PR timeline informative.

**WorldSnapshot fields:**
- `reviewer_in_progress_comment_ids: HashMap<u64, u64>` — Maps PR number to the GitHub comment ID of a dangling "Review in progress" placeholder comment. Collected during `collect_world_snapshot()` using `reviewer_placeholder_cache` (TTL: 120s for all entries, both positive and negative). Used by `decide_stuck_reviewer_restarts` to select the shorter stuck threshold for placeholder PRs, and by health functions to emit `UpdatePrComment` effects marking abandoned placeholders.
- `reviewer_restart_counts: HashMap<u64, u32>` — Maps PR number to the number of times a reviewer has been restarted for that PR.
- `reviewer_escalations_posted: HashSet<u64>` — Tracks PRs for which a max-restart escalation warning has already been posted (prevents repeated spam).

**DaemonState fields:**
- `reviewer_placeholder_cache: Mutex<HashMap<u64, (Option<u64>, Instant)>>` — Cache for `pr_in_progress_placeholder_comment_id()` lookups. Maps PR number to `(comment_id_or_none, checked_at)`. Both positive and negative entries expire after `PLACEHOLDER_CACHE_TTL_SECS` (120s). Cleared when `mark_reviewed_pr()` is called to ensure freshness after a review is posted.

**Effect:**
- `Effect::UpdatePrComment { comment_id, repo_full_name, new_body }` — Patches an existing GitHub issue comment via `gh api --method PATCH`. Used to update stale "Review in progress" placeholder comments when a reviewer is restarted due to being stuck or dead.

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
