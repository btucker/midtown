> Back to [README](../README.md)

# How It Works

## Daemon

The daemon is the central coordinator. It runs an event-driven state machine that collects an immutable snapshot of the world each tick, makes pure decisions about what should happen, and then executes the resulting effects. This strict separation between decision logic and side effects keeps the core testable.

The daemon handles:
- Coworker lifecycle (spawning, health checks, stuck detection, shutdown)
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

Coworkers are named after Manhattan avenues: lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon.

## Channel Leads

Channel leads are headless Claude Code sessions attached to individual topic channels. Where coworkers are temporary implementers that come and go with tasks, channel leads are long-lived domain experts that accumulate context across conversations.

**Role:** A channel lead brainstorms, maintains living design documents, answers domain questions, and tracks awareness of active tasks and PRs in its channel. It does not write code, open PRs, or create tasks. When implementation work is needed, it escalates to @lead.

**Message routing:** When a user posts to a topic channel (any non-main channel), `handle_channel_post` in `src/daemon/mod.rs` nudges the channel lead for that channel via `SessionManager::send_message`. If no channel lead session is alive for that channel, the message is silently skipped — it remains in the channel log and is available when the channel lead next starts up. Main channel behavior is unchanged. Note: `route_mentions()` is intentionally disabled for topic channels — user `@coworker` and `@all` mentions in topic channels are silently dropped; only the channel lead nudge path is active.

**System prompt:** Channel leads use the `agents/channel-lead.md` template, instantiated with `{channel_name}` and `{domain_context}` via `channel_lead_system_prompt()` in `src/agents.rs`.

**Coworker guidance:** Coworkers are instructed to `@{channel-name}` for domain questions (e.g., architecture, design decisions) and to reserve `@lead` for coordination, task, and priority questions.

## Channel Sync

Coworkers stay synchronized via a Claude Code Stop hook. When Claude pauses, the hook reads new channel messages and checks for unclaimed tasks. This means coworkers automatically receive updates at natural pause points.

## Channel Leads

Topic channels can have a dedicated channel lead — a headless Claude Code session with persistent context for a specific area of the codebase. Channel leads are spawned from the `agents/channel-lead.md` system prompt template and provide domain expertise to coworkers working in their channel. Coworkers are instructed to ask `@channel-lead` for domain questions before escalating to `@lead`, forming a three-tier question hierarchy: channel lead → lead → peer coworker.

## Mailbox Messaging

In addition to the shared channel, the daemon can deliver targeted messages to individual coworkers via the Claude Code agent teams mailbox protocol. Messages are written as JSON to `~/.claude/teams/{team-name}/inboxes/{agent-name}.json` using atomic file operations with mkdir-based locking for safe concurrent access.

## Worktree Lifecycle

When a coworker is called in, midtown creates a detached git worktree at the current HEAD. The coworker creates a feature branch and works independently. When the coworker shuts down, worktrees with no commits and no uncommitted changes are automatically cleaned up along with their branches. Worktrees with work in progress are preserved.

## GitHub Integration

The daemon receives real-time GitHub events via webhooks (PR creation, reviews, check runs) verified with HMAC-SHA256 signatures. PR polling runs as a backstop for missed webhook deliveries and handles time-based concerns like merge conflict detection and stuck PR identification.

## Webhook Ports

Each project daemon runs its own webhook server for GitHub integration. Port 47022 is reserved for the shared multi-project webserver. Per-project daemons auto-assign ports starting at 47023, persisting the assignment in the project's `config.toml` for stability across restarts.

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
                                      → kanban.data RPC → TUI tool activity display
```

- **Types** (`mod.rs`): `UniversalItem`, `ItemKind`, `ContentPart`, `ItemStatus` — agent-agnostic, extensible to other providers.
- **Claude converter** (`claude.rs`): Pure function `extract_tool_events()` that extracts both `tool_use` content blocks from `StreamEvent::Assistant` events and `tool_result` blocks from `StreamEvent::User` events. Each tool call is emitted with a `semantic_header` (human-readable description of the operation) and each tool result carries success/error status.
- **Integration** (`daemon/stream.rs`): `process_universal_events()` accepts the `channel_lead_sessions` map and emits `BroadcastUniversalItems` effects for the main lead (channel=None) and for each active channel lead (channel=Some(channel_name)). Coworker tool calls are never broadcast.
- **Broadcast**: The `BroadcastUniversalItems` effect sends `WebUpdate::UniversalItems` to all connected WebSocket clients and updates `DaemonState.recent_tool_items` (a `RwLock<HashMap<String, Vec<UniversalItem>>>`, capped at `MAX_TOOL_ITEMS_PER_AGENT=20` items per agent). Agent name and optional channel are carried at the envelope level (`UniversalItemsData`). The web UI stores items keyed by channel name (`'midtown'` for the main lead, or the topic channel name for channel leads) so each channel view shows only the relevant tool calls.
- **TUI rendering**: The TUI polls `kanban.data` which calls `collect_tool_activity()` to serialize `recent_tool_items`. The TUI renders a compact activity strip at the bottom of the chat pane showing the most recent tool calls per active agent, using `semantic_header` for tool call labels and "✓ ok" / "✗ error" for tool results.
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
