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
| PR open → link task (`SetTaskPr`) | Webhook | Polling repairs missed links (`collect_pr_task_link_effects`) |
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

6. **Session recovery** — `recover_from_session_records()` generates `ResumeCoworker` effects for each resumable session (those with `is_running=true` and `resume_on_startup=true`). The old process is NOT killed here — it dies naturally from the broken pipe when its previous daemon's handles are closed. A fresh `claude --resume <session_id>` process is spawned to continue the session.

7. **Channel lead mapping recovery** — `recover_channel_lead_session_mappings()` reconstructs `channel_lead_sessions` from persisted `SessionRecord`s before stale-session cleanup. It filters to root channel leads (`coworker_type=channel-lead`, no `bound_thread_id`, and `resume_on_startup=true`) and keeps the newest session per channel. Mappings are restored independently of `is_running`.

8. **Stale flag cleanup** — `clear_stale_running_sessions()` clears the `is_running` flag for any session not included in the recovered set. This includes channel lead records that are not part of coworker recovery, so stale "alive" flags do not block dispatch or resume logic.

## Coworkers

Each coworker runs as:

- A headless Claude Code process (`claude -p --output-format stream-json`) managed by the daemon's `SessionManager`
- In an isolated git worktree (no merge conflicts during development)
- With `--add-dir` worktrees for additional repos in multi-repo projects
- Nudges are delivered via stdin JSON, and health is monitored via stdout stream events

### Direct CLI Boot (Headed Sessions)

In addition to daemon-managed headless sessions, users can launch **headed (interactive terminal) sessions** directly from the CLI:

- `midtown lead [--channel <name>]` — boots a headed lead session via `exec()`, replacing the CLI process with the Claude TUI. Uses `LaunchConfig::lead()` to resolve model/provider/auth, writes system prompt and settings to temp files, then calls `to_shell_command()` to build the full `sh -lc` invocation.
- `midtown coworker [--task <id>]` — boots a headed coworker session in a task worktree. If no `--task` is specified, presents a TUI picker for unresolved tasks. Creates/reuses worktrees via `WorktreeManager::create_task_worktree()`.

Both use `SessionMode::Resume` (`--continue`) so they resume an existing Claude session or start fresh. The `exec()` pattern means the Midtown process is fully replaced — no parent process lingers. Initial prompts are written to temp files and passed as positional args via `$(cat /path)`.

### HeadlessSession I/O Architecture

`HeadlessSession` (`src/headless.rs`) manages the child process and exposes a typed event stream. Claude sessions use a **background reader** pattern to avoid OS pipe-buffer stalls:

- On spawn, two `tokio::spawn` tasks are created — `claude_stdout_reader_loop` and `claude_stderr_reader_loop` — each owning a `BufReader` over the child's piped stdout/stderr.
- Each task continuously calls `read_line()` and forwards parsed events or raw lines into **unbounded `mpsc` channels** (`stdout_rx` / `stderr_rx` stored in `HeadlessSession`).
- `next_claude_event()` does a single `rx.recv().await` — a simple channel receive. Blank lines and `StreamEvent::Unknown` events are filtered in the reader task, not in the hot path.
- `drain_stderr()` waits up to 10ms for any line the background task is mid-read, then drains up to 100 lines non-blocking.
- **Detach-on-drop**: When `detach_on_drop` is set (daemon restart path), the `Drop` impl spawns drain tasks to keep the channel receivers alive. Without this, dropping the receivers would cause the reader tasks to exit, closing the pipe FDs and sending SIGPIPE to the child.

This mirrors the Codex session pattern (`read_stdout_loop` / `read_stderr_loop` in `CodexSharedRuntime`) and ensures the child process never blocks on a full 64 KB kernel pipe buffer regardless of output volume.

**Codex runtime reuse:** In `src/headless.rs`, `CODEX_RUNTIME` is a `HashMap` keyed by profile (`codex-runtime|<CODEX_HOME>`), not a single global process. The daemon now shares one app-server per profile and reuses it across sessions, while isolating runtime state between profiles.

### Session-Centric Model

The daemon uses a **session-centric model** where Claude Code sessions (keyed by session ID) are the primary coordination entity. Names are ephemeral labels drawn from an LRU pool.

**NamePool** (`src/name_pool.rs`): Manhattan avenue names (lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon) are managed in an LRU queue. When a session spawns, it allocates a name from the front of the queue. When it shuts down, the name returns to the back. Preferred name hints allow a resumed session to get its previous name when available, preserving branch and worktree continuity. Name allocation and release both clear the agent's mailbox inbox to prevent message bleed between sessions (see Mailbox Messaging).

**SessionRecord** (`src/daemon/state.rs`): Each session is tracked by a `SessionRecord` containing session ID, task ID, current and preferred names, worktree path, branch, PR number, and running state. Records persist across daemon restarts in `persistent_state.json`.

**Resume fallback path** (`spawn_with_resume_fallback` in `src/daemon/effects.rs`): Resume effects first attempt the requested resume mode and automatically fall back to a fresh spawn if resume fails. The fallback injects a handoff prompt with the prior context so the new session can continue the work with minimal context loss instead of dropping the request.

**Dispatch** (`src/daemon/dispatch.rs`): Two dispatch paths handle session recovery. **Path 1** (`dispatch_via_sessions_with_task_lookup`, called from `dispatch_via_sessions()`) examines in-progress tasks with session records. For stopped sessions, it emits `SpawnSession` effects with `resume=true` and the session's preferred name, unless the coworker is an active reviewer or the session was recently recovered (per-session cooldown prevents re-recovery spam). This replaces the legacy orphan-recovery pattern with a unified session-aware dispatch path.

**Path 2** (`spawn_for_pending_tasks_excluding`, `src/daemon/dispatch.rs`): Handles pending tasks whose stopped session has no active task assignment. Before building the `LaunchConfig`, it validates the recorded `working_dir` with `.exists()`: if the worktree has been cleaned up since the session last ran, Path 2 falls back to a freshly computed worktree path (from `prepare_task_worktree`) and logs a warning. The chosen path is passed as `working_dir` in the `SpawnSession` effect. On successful spawn, the `SpawnSession` handler in `effects.rs` updates `record.working_dir` with the actual path used — ensuring the stale path never persists into the next tick and preventing repeated fallback log spam.

**In-memory reverse maps** on `DaemonState`:
- `name_to_session` / `session_to_name` — bidirectional name↔session lookup
- `task_to_session` — task→session mapping for dispatch decisions

On daemon startup, the `NamePool` is restored from persisted session records: names with active sessions are marked allocated, the rest are available in LRU order.

**Daemon-controlled session IDs**: `spawn_coworker()` returns `Result<String>` — the session ID used for the spawn. For fresh sessions, a UUID is generated upfront and passed to the CLI via `--session-id`, so the daemon knows the session ID immediately at spawn time. For resumed sessions, the existing session ID from `SessionMode::ResumeSession` is reused. This eliminates the race window where `name_to_session`, `session_to_name`, and `channel_lead_sessions` were empty until the init StreamEvent arrived. All callers of `spawn_coworker` (effects.rs handlers, `expedite_lead_respawn_on_user_message`) capture the returned session ID and update their state eagerly.

**Auth Profile Pool** (optional): When `[execution].coworker_profiles` (or `reviewer_profiles`, `channel_lead_profiles`) is set in config, `spawn_coworker()` selects an auth profile using an LRU-among-available strategy before resolving `auth_profile_dir`:
1. Filter profiles where `ProfileState.is_usage_limited` is `true` in `DaemonPersistentState.profile_pool_state`.
2. Among available profiles, pick LRU by `ProfileState.last_used_at` (never-used profiles preferred with `last_used_at = None`).
3. If all profiles are limited, fall back to the single-profile path (existing behavior).
4. On success, update `last_used_at` in `profile_pool_state`, record `session_name → profile_email` in `session_profile_map`, and save state.

**Limit detection and clearing**: When `check_for_usage_limits()` detects a session has hit its usage limit, it looks up the session's profile in `session_profile_map` and emits a `MarkProfileLimited` effect — setting `is_usage_limited: true` and recording `usage_limit_reset_at` in `profile_pool_state`. Limit clearing is explicit: when the scheduled `UsageLimitNudge` handler fires (after the reset timer), it emits `ClearProfileLimit`, which sets `is_usage_limited: false` and clears `usage_limit_reset_at`. A profile with a past `reset_at` remains limited until this effect fires — the pool selection logic reads only `is_usage_limited`, never the timestamp directly.

`DaemonPersistentState.profile_pool_state` (`HashMap<String, ProfileState>`) persists per-profile usage state across daemon restarts. `ProfileState` tracks `is_usage_limited`, `usage_limit_reset_at`, and `last_used_at`. The in-memory `session_profile_map` (`HashMap<String, String>`) tracks `session_name → profile_email` for limit attribution; it is not persisted (profiles become available again on daemon restart).

**Pool management API**: The web UI can toggle profiles in and out of the coworker pool via `POST /api/auth/pool-toggle` (REST) or the `auth.pool-toggle` RPC method. Request: `{ profile, enabled, provider? }`. When `enabled=true`, the profile is added to `execution.coworker_profiles` in project config (validates that the profile exists first). When `enabled=false`, it is removed. If `coworker_profiles` is unset (`None`) and `enabled=false`, the config is not modified — this avoids creating an explicit empty list that would shadow inherited global pool entries. After mutation, an ops-channel broadcast notifies WebSocket clients of the change.

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
- **Dispatch**: coworker-limit checks use `is_project_lead()` (`src/daemon/dispatch.rs`)
- **Effects**: auto-detach suffix check and skip-filter use `state.repo_name` (`src/daemon/effects.rs`)
- **Stop-time key**: `coworker_stop_times` entries for the lead are keyed by `repo_name.to_lowercase()`
- **Attached key**: `attached_coworkers` entries for the lead are keyed by `repo_name` (lowercase)

All lead-identity checks use `helpers::is_project_lead(name, repo_name)` (`src/daemon/helpers.rs`), which accepts both the canonical repo name and the legacy `"lead"` string for backward compatibility with older sessions. This single helper is used by `rpc_kanban.rs`, `dispatch.rs`, `mod.rs`, and `pr.rs` to ensure consistent behavior. The `coworkers.status` API uses `is_lead_health_active(health, repo_name)` to check both health-map keys.

**Backward-compat guard — `is_project_lead()`**: In `rpc_kanban.rs`, `is_project_lead(name, repo_name)` encapsulates the two-condition check for lead sessions: it returns `true` if the name equals the repo name *or* the legacy literal `"lead"` (case-insensitive). All code that needs to identify or exclude the lead session should call this helper rather than inlining the two-condition check. Current callers: `handle_coworker_list()` (`rpc_coworker.rs`), `handle_status()` (`rpc_status.rs`), `handle_coworker_report_state()` (`rpc_coworker.rs`), and the kanban build path in `rpc_kanban.rs`.

## Provider Resolution

Each session role resolves its AI provider via `get_execution_provider_for_role()` in `src/config.rs`. The resolution chains are:

- **Lead**: `execution.project_lead_provider` → `execution.lead_provider` → `Claude` (default)
- **Channel Lead**: `execution.channel_lead_provider` → `execution.lead_provider` → `Claude` (default)
- **Coworker**: `execution.coworker_provider` → `Claude` (default)
- **Reviewer**: `execution.reviewer_provider` → `Claude` (default)
- **HeadlessExecute** (`oneshot.execute`): `execution.headless_execute_provider` → `execution.specialized_provider` → `Claude` (default)

Review source strategy is controlled separately by `execution.review_mode`:
- `local`: daemon spawns local reviewer coworkers
- `github_app`: daemon does not spawn local reviewers, and waits for formal GitHub reviews
- `both`: local reviewer spawning remains enabled and formal GitHub reviews also count

This means `lead_provider` acts as a shared fallback for both the Project Lead and Channel Leads. Setting `project_lead_provider` overrides only the Project Lead's provider without affecting channel leads, and vice versa for `channel_lead_provider`. The resolved provider is stored in `LaunchConfig.auth_provider` and is also used to derive the default model via `default_model_for_provider_role()`.

## Channel Leads

Channel leads are headless Claude Code sessions attached to individual topic channels. They are on-demand domain experts — spawned when triggered, shut down when idle, and resumed within a daemon run.

**Role:** A channel lead brainstorms, maintains living design documents, answers domain questions, and tracks awareness of active tasks and PRs in its channel. It does not write code, open PRs, or create tasks. When implementation work is needed, it escalates to `@{project_name}`.

**Lifecycle:** Channel leads are spawned on-demand by these triggers:
- **User message** in the channel (via `handle_channel_post`)
- **Task created** in the channel (via `handle_task_create`)
- **Insight posted** to the channel (via `handle_insight_report`)
- **Explicit nudge** (@mention routing, task feedback)

**Archived-channel redirect:** When a task is created with `--channel <name>` pointing to an archived channel (e.g., `daemon`), `handle_task_create` redirects the task to the ops channel via `resolve_effective_task_channel()`. The effective channel is stored in both the task JSON and `ps.task_channel`, ensuring all downstream routing (announcement posting, `NudgeChannelLead`, `MIDTOWN_CHANNEL` injection, insight posting, `handle_task_metadata`) uses the routable channel. If the ops channel is itself archived, the redirect falls back to the main channel.

All triggers use the `NudgeChannelLead { channel_name, reason }` effect. The execution layer in `effects.rs` routes with session-id-first behavior to avoid name collisions: it tries `send_message_to_session_id()` using the stored channel mapping; if stale/missing, it refreshes from the active named session; if resume is possible, it uses `spawn_with_resume_fallback(...)` and then sends to the resumed/fresh session; otherwise it spawns fresh and persists the new session ID.

The project lead is the channel lead for the main channel — `NudgeChannelLead` routes to the project lead's dual-path nudge (headless session manager or headed intercom) when the channel is the default channel.

Channel leads participate in normal idle shutdown (same timeout as coworkers). The `channel_lead_sessions` map is rebuilt at startup from session records and then maintained during runtime; `WakeReason` (in `src/daemon/wake_reason.rs`) captures why a session is being woken and provides formatting for both nudge messages and initial prompts.

Note: `route_mentions()` is intentionally disabled for topic channels — user `@coworker` and `@all` mentions in topic channels are silently dropped; only the channel lead nudge path is active.

**System prompt:** Channel leads use the `agents/channel-lead.md` template, instantiated with `{channel_name}`, `{domain_context}`, and `{project_name}` via `channel_lead_system_prompt()` in `src/agents.rs`.

**Domain context from notes:** The `{domain_context}` variable is populated by `load_channel_notes()` in `src/channel.rs`, which reads all `.md` files from `channels/<name>/notes/`, concatenates them with filename-derived headers, and caps total size at 100 KB. This is called at all 4 channel lead spawn sites (3 in `effects.rs`, 1 in `cli/lead.rs`) so that channel leads always start with their accumulated domain knowledge. Insight nudges include a reminder to save important knowledge to notes, completing the feedback loop.

**Coworker guidance:** Coworkers are instructed to `@{channel-name}` (e.g., `@daemon-core`) for domain questions and to reserve `@{project_name}` for coordination, task, and priority questions.

### Forked Sessions (Thread-Specific Channel Leads)

Channel leads can fork themselves into thread-specific sessions via the `session.fork` RPC (`midtown session fork <thread-parent-id>`). A forked session inherits the parent's conversation context and gets an independent session ID bound to a specific thread (Claude/z.ai use `--resume <parent-id> --fork-session`; Codex uses `thread/fork`).

**Root session as router:** The root session stays lightweight — it handles top-level messages and decides when to fork. Once a fork exists for a thread, subsequent replies in that thread bypass the root session entirely and route directly to the fork.

**Daemon auto-fork (primary path):** When a user posts a new top-level message to a topic channel, `handle_channel_post` calls `create_fork_session` before sending the nudge. The fork is bound to the message's thread anchor ID so the channel lead receives the nudge already inside the fork session — it just replies directly. `create_fork_session` is a `pub(super)` shared helper in `rpc_session.rs`, callable from both `handle_session_fork` (explicit CLI call) and `handle_channel_post` (auto-fork path). The `channel_hint` parameter lets the auto-fork path supply the known channel name even when session records are stale. `create_fork_session` uses `try_lock()` on `persistent_state` internally to avoid blocking the channel post path when the lock is held.

**Graceful fallback:** If no channel lead session is registered (first message to a new channel), or if `persistent_state.try_lock()` is contended, auto-fork is skipped and `NudgeChannelLead` spawns the channel lead as before. The next user message will succeed once the lead is running. `session fork` is still documented in `channel-lead.md` as a fallback for when the daemon was not running when the message arrived.

**Thread routing priority:** When a message arrives with `thread_parent_id` set, `handle_channel_post` checks `topic_sessions[thread_parent_id]` first. "pending" entries are filtered out (a concurrent auto-fork is in progress but not yet ready) — the reply falls back to `NudgeChannelLead` rather than producing a nudge with an invalid session ID. Once the fork completes, subsequent replies route to the real fork session.

**`session fork` during auto-fork spawn window:** If the channel lead calls `midtown session fork` while the daemon's auto-fork is still spawning (the "pending" sentinel is set), `handle_session_fork` returns `{pending: true, thread_parent_id: "..."}` instead of an error, so the lead can distinguish "retry shortly" from a hard spawn failure.

**Data flow:**
- `topic_sessions` (in-memory `Mutex<HashMap<String, String>>`) maps `thread_parent_id → fork_session_id`. Entries are "pending" (spawn in progress) or a real session ID. Used by both auto-fork and thread-reply routing in `handle_channel_post`.
- `fork_bound_threads` (in-memory `Mutex<HashMap<String, String>>`) maps `fork_name → thread_parent_id`. Used by the output binding path in `handle_channel_post` to auto-tag forked session posts with their bound thread (avoids the async `persistent_state` lock on the hot path).
- `DaemonPersistentState.task_thread_id` maps `task_id → thread_parent_id`. Populated in two ways: (1) explicitly via `--thread-id` on `midtown task create` (the CLI defaults to `$MIDTOWN_BOUND_THREAD_ID` inside fork sessions), or (2) automatically defaulted to the task's announcement message ID when no explicit thread ID is provided. This ensures every task's coworker posts route to the task announcement thread by default. `SpawnSession` reads this mapping to set `bound_thread_id` on the spawned coworker's `SessionRecord`.
- `SessionRecord.bound_thread_id` (persisted) stores the binding on each session so restarts can rebuild the cache.
- `name_to_session` / `session_to_name` reverse maps are backfilled in `create_fork_session` since fork sessions (--resume --fork-session) never emit `system/init` and the session ID is pre-assigned via `--session-id`.

**Architectural invariants:**
- Fork sessions are NOT registered in `CoworkerManager`. They bypass `spawn_coworker()` entirely, which means they are excluded from idle-shutdown evaluation, orphan recovery, and coworker status tracking.
- The `topic_sessions` guard uses an atomic check-and-reserve pattern (inserting a "pending" sentinel) to prevent duplicate forks for the same thread. On spawn failure, the sentinel is removed so the slot is available for retry.
- `topic_sessions` is cleared on daemon restart, so fork sessions themselves do not survive across daemon lifetimes.
- `fork_bound_threads` is rebuilt on startup from persisted `SessionRecord.bound_thread_id` entries, which keeps thread-bound coworkers routed correctly across restarts (including auto-binding spawned tasks via `task_thread_id`). Entries created directly by `create_fork_session` remain ephemeral.

## Channel Storage Layout

Each channel is stored as a directory under `~/.midtown/projects/<repo>/channels/`:

```
channels/
  <project>/                        # main project channel (named after the repo, e.g. "offload")
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
- `channel.jsonl` → `channels/<project>/history/current.jsonl` (V0→V3)
- `channels/<name>.jsonl` → `channels/<name>/history/current.jsonl` (V2→V3)
- `cursors/<agent>.json` → deleted (cursors are now session-scoped and ephemeral)

Migration runs once per `base_dir` per process (via `OnceLock`) and is idempotent.

### Multi-File History Reads

Channel reads span all `.jsonl` files in the history directory — date-named archives plus `current.jsonl` — so history is preserved across daily rotation. The key helpers live in `src/channel.rs`:

- **`list_all_history_files()`** — Lists all `.jsonl` files in the history directory. Date-named archives (`YYYY-MM-DD.jsonl`) are sorted ascending (oldest first), with `current.jsonl` appended last. Temp files (`.rotating`) are excluded.

- **`read_messages_from_file()` / `read_messages_from_file_async()`** — Reads all messages from a single `.jsonl` file. Each file is locked individually (shared lock with bounded retries — up to 10 attempts at 50ms intervals). The async variant uses `tokio::time::sleep` instead of `std::thread::sleep` to avoid blocking the runtime.

**Read paths:**

| Method | Behavior | File ordering |
|---|---|---|
| `read_all()` | Reads every history file, sorts all messages by timestamp | Forward (oldest archive → current) |
| `read_last_n_messages()` | Reads files in reverse order, stops early once ≥N messages collected | Reverse (current → oldest archive) |
| `read_messages_before_position()` | Reads all files, skips the tail `position` messages, returns next N | Forward (all files) |
| `read_messages_from_position()` | Byte-offset seek in `current.jsonl` only (streaming hot path) | Single file |

**Pagination uses message counts, not byte offsets.** `read_last_n_messages` returns a `start_position` representing the count of messages loaded from the tail. `read_messages_before_position` uses this count to compute the correct page across all archive files. If `start_position == 0`, all history has been loaded.

**Dual-purpose cursors:** `set_cursor_to_end()` sets two distinct values:
- `position` — byte offset in `current.jsonl`, used by `read_messages_from_position()` for streaming new messages (the hot path, no lock needed due to `O_APPEND` atomicity).
- `last_message_id` — drawn from all history files (via `read_last_n_messages(1)`), used for unread-count calculations so rotation doesn't cause a false spike in unread messages.

**Rotation** (`rotate()`) moves messages older than `retain_minutes` from `current.jsonl` to `YYYY-MM-DD.jsonl` archives. It acquires an exclusive lock on `current.jsonl`, partitions messages by timestamp, appends archived messages to the date-named file (creating or appending), writes retained messages to a `.rotating` temp file, and atomically renames it over `current.jsonl`. After rotation, all cursor files are reset via `cursor.reset()` — both `position` and `last_message_id` are cleared — because byte offsets in `current.jsonl` have changed. Note: the exclusive flock is held on the original file descriptor; after `atomic_rename` replaces the inode, new readers opening `current.jsonl` by path are not blocked by the lock. The cursor reset loop runs after the rename, so there is a brief window where a concurrent reader could observe a partially-reset cursor set.

**Cursor state after rotation — by consumer:**
- **Active channel (chat TUI):** The TUI keeps an in-memory `cursor_last_message_id` that survives rotation. `refresh_unread_counts()` uses this in-memory ID with `read_all()` (which spans archives + current), so it finds the pre-rotation message in the archives and computes the correct unread count. No false spike.
- **Streaming reads (`read_since_cursor()`):** After reset, reads from `position: 0` in `current.jsonl`, re-reads retained messages, and calls `cursor.update()` to restore `position` and `last_message_id`. This rebuilds the streaming cursor correctly, though `last_message_id` reflects only the last message in `current.jsonl` (not the global last across archives).
- **Inactive/background channels:** If no agent polls a channel between rotation and the next `refresh_unread_counts()`, all disk cursors have `last_message_id: None`. The unread-count path falls through to "all messages are unread," causing a transient spike until the next `read_since_cursor()` or `set_cursor_to_end()` rebuilds the cursor on disk.

**Channel RPC methods** (handled by `src/daemon/rpc_channel.rs`):
- `channel.post` — Append a message to a channel; handles `/me` actions, @mention routing, review note deduplication
- `channel.read` — Read messages from a channel (supports `all`, `last`, `since`, and per-channel filtering)
- `channel.create` — Create a new channel directory; idempotent (no-op if channel already exists)
- `channel.archive` — Rename `channels/<name>/` to `channels/<name>.archived/`; returns an error if the channel does not exist or if archiving the project's main channel
- `channel.unarchive` — Rename `channels/<name>.archived/` back to `channels/<name>/`; returns an error if the channel is not archived or if another active channel of the same name exists
- `channel.rename` — Rename `channels/<old>/` to `channels/<new>/`; updates `task_channel`, `channel_lead_sessions`, and `sessions` in persistent state; shuts down the old channel lead session; returns an error if the old channel does not exist, the new name is invalid/already exists, or if renaming the project's main channel
- `channel.list` — Return all channels, optionally including archived ones

> Note: Channels are no longer auto-archived when all tasks complete. Archiving and unarchiving are explicit user actions via the CLI/RPC methods above.

## Channel Sync

Coworkers stay synchronized via a Claude Code Stop hook. When Claude pauses, the hook reads new channel messages and checks for unclaimed tasks. This means coworkers automatically receive updates at natural pause points.

## Nudge System

Nudge decisions are made in `src/rules.rs` (`decide_interrupt_nudges`, `decide_prompt_nudges`) using `CooldownTracker` for per-coworker cooldowns and `CoworkerPhase` for deduplication (Idle → Prompted → Interrupted). Delivery is via `Effect::NudgeCoworker` / `Effect::NudgeLead` in `src/daemon/effects.rs`:

- **Project Lead nudges**: Delivered through headed intercom queues (`headed.register/poll/ack`) with tmux fallback
- **Coworker nudges**: JSON streaming via `SessionManager` for headless sessions

**Review content embedding**: PR feedback nudges (GreenWithFeedback, ReviewComplete, ChangesRequested, Approved, ReviewComment) embed the full review body inline via `format_review_content()` in `helpers.rs`. This fetches both formal GitHub reviews and Midtown coworker issue-comment reviews (detected by `text_contains_review_signature()`), so the nudged coworker sees all feedback without running extra `gh` commands. On the polling path (`poll_prs_for_issues`), review content is pre-fetched in bulk before decision functions to keep I/O out of the decision phase. Webhook handlers call `fetch_review_content()` directly since they're already event-driven I/O paths.

## Mailbox Messaging

In addition to the shared channel, the daemon can deliver targeted messages to individual coworkers via the Claude Code agent teams mailbox protocol. Messages are written as JSON to `~/.claude/teams/{team-name}/inboxes/{agent-name}.json` using atomic file operations with mkdir-based locking for safe concurrent access.

**Inbox lifecycle**: Inboxes are cleared at two points to prevent message bleed across sessions: (1) when a name is allocated from the `NamePool` during `SpawnSession` (before the new session starts), and (2) when a session releases its name on shutdown. This ensures a newly-allocated name never inherits stale unread messages from a previous session that held the same name. All inbox operations — writes, reads, and clears — acquire the same mkdir-based lock (`{agent-name}.json.lock`) to prevent races.

**PR review gate warnings**: Two mailbox messages are sent to PR authors to prevent premature auto-merge:

1. **PR-opened warning** (`pr_opened_author_warning`): Delivered immediately via the `pr_opened` webhook handler when a non-draft PR is opened. Sent before a reviewer is assigned, ensuring the author is warned even when the reviewer spawn is delayed (e.g., coworker limit temporarily hit). Gated on `needs_review.is_some()` so draft PRs don't receive it.

2. **Reviewer-spawned notification** (`reviewer_spawned_author_warning`): Delivered via `SpawnCoworkerWithCallbacks.on_success` in `collect_reviewer_effects_with_source` when the reviewer actually spawns. Identifies the reviewer by name and reinforces the hold on auto-merge. This fires only on successful spawn, while the PR-opened warning covers the gap when spawn is delayed.

Together these create a layered defence: the early warning reaches the author at PR creation time, and the spawn notification confirms who is reviewing.

## PR Merge Gating

When a coworker calls `midtown pr merge --pr <N>`, the daemon runs a pre-gate and three gates before enabling auto-merge:

**Pre-gate — Reviewer active**: Checks `get_reviewer(pr_number)` for raw assignment presence in `pr_reviewers`. If a reviewer coworker is assigned to the PR, the merge is rejected immediately — before any API calls. Uses non-expiring assignment presence (not the 600s timeout from `is_assigned()`), so long-running reviews are never bypassed. The assignment is only removed when the review actually completes or the PR is closed. This is the only gate that cannot be bypassed by prompt-based instructions.

1. **Gate 1 — Review completed**: Checks `is_pr_reviewed()` which looks in the persistent `reviewed_prs` set. A PR is only marked as reviewed when the **assigned reviewer** posts the review — bot comments, unrelated coworkers, and other noise are filtered out via `review_author_matches()` (body-based identity extraction from `<!-- midtown: name -->` frontmatter or review signatures). Formal reviews with strong states (APPROVED / CHANGES_REQUESTED) are accepted even with empty bodies since these are deliberate human actions. The `WebhookEvent.review_author` field carries the extracted identity for the webhook path; the polling path passes `assigned_reviewer` from `pr_reviewers` to `pr_has_completed_review_uncached()`. When no reviewer is assigned, any valid review is accepted (backward-compatible).

2. **Gate 2 — CI passing**: Checks `statusCheckRollup` from `gh pr view` and also verifies `reviewDecision != "CHANGES_REQUESTED"`. The PR must be in `OPEN` state (merged/closed PRs are rejected before gate checks).

3. **Gate 3 — Feedback addressed**: For each review comment ID in `pr_review_comment_ids`, checks that a subsequent PR comment contains `<!-- addresses-review: {id} -->`. Unaddressed feedback blocks the merge.

**Review comment ID collection**: Review comment database IDs are collected via two paths:
- **Webhook path** (primary): When `handle_issue_comment` detects a code review (via `is_review_comment()`), the `WebhookEvent.review_comment_id` field carries the comment's database ID. The daemon handler persists it via `add_review_comment_id()`.
- **Polling fallback**: When `is_pr_reviewed()` first detects a review via `pr_has_completed_review_uncached()`, it calls `fetch_review_comment_ids()` to retrieve comment IDs from the GitHub REST API.

**State**: `pr_review_comment_ids: HashMap<u64, Vec<u64>>` in `GitHubState` maps PR numbers to lists of review comment database IDs. Persisted in `daemon-state.json`.

**Effect**: On success, `Effect::MergePr` calls `gh pr merge --squash --auto` with `current_dir` set to the repo path.

**Pre-gate — Reviewer active**: Before the three gates, the RPC handler checks `get_reviewer(pr_number)`. If a reviewer coworker is still assigned, the merge is hard-blocked. This prevents the PR #1624 incident where a merge happened while the reviewer was still working.

**Workflow event gate** (`pr.approved`): The `PrApproved` workflow event is gated on the same reviewer check in `pr_action_to_effects`. When `PrContext.has_active_reviewer` is true (reviewer assigned AND review not yet cached), `PrApproved` is suppressed so workflow scripts don't prematurely nudge the author to merge. The `Approved` nudge cooldown is cleared when the reviewer finishes (in `collect_reviewer_effects`) so PrApproved fires promptly on the next tick.

## Worktree Lifecycle

When a coworker is called in, midtown creates a detached git worktree at the current HEAD. The coworker creates a feature branch and works independently. When the coworker shuts down, worktrees with no commits and no uncommitted changes are automatically cleaned up along with their branches. Worktrees with work in progress are preserved.

## Task Completion

Tasks complete through two paths depending on whether they produce a PR:

1. **PR tasks** (most common): The coworker opens a PR and reports `midtown state idle`. The daemon auto-completes the task when the PR merges, via webhook or polling fallback. The task stays `in_progress` until merge — `task_has_open_pr()` in `rpc_coworker.rs` checks both `pr_author_sessions` (in-memory) and the `task.pr` field on disk (survives daemon restarts). When using the disk fallback, the PR state is verified via `gh pr view` since `task.pr` is never cleared when a PR is closed — without this check, a closed/superseded PR would incorrectly block task completion.

2. **No-PR tasks** (ops, release management, investigations): The coworker reports `midtown state completed`. Since no PR exists, the daemon completes the task directly on disk (same as `task.done` RPC), clears `blocked_by` dependencies, and marks the worktree as completed. This avoids the respawn loop that occurs when a task stays `in_progress` with no PR — dispatch would repeatedly recover and respawn the coworker.

## GitHub Integration

The daemon receives real-time GitHub events via webhooks (PR creation, reviews, check runs) verified with HMAC-SHA256 signatures. PR polling runs as a backstop for missed webhook deliveries and handles time-based concerns like merge conflict detection and stuck PR identification.

## Webhook Ports

Each project daemon runs its own webhook server for GitHub integration. Port 47022 is reserved for the shared multi-project webserver. Per-project daemons auto-assign ports starting at 47023, persisting the assignment in the project's `config.toml` for stability across restarts.

## Thread Storage

Thread replies are stored in the same JSONL channel file as top-level messages, tagged with a `thread_parent_id` field referencing the parent message's ID. There is no separate index — threads are filtered in memory at read time by comparing `thread_parent_id` against the queried parent ID. Top-level messages have `thread_parent_id` omitted (serialized with `skip_serializing_if = "Option::is_none"` for backward compatibility with existing channel logs).

The `/api/channels/history` endpoint accepts an optional `thread_parent_id` query parameter:
- Absent: returns only top-level messages (where `thread_parent_id` is `None`), with `reply_count` and `last_reply` metadata when replies exist
- Present: returns only thread replies matching the given parent ID

When a thread reply is posted via `midtown channel post --thread <id>`, the daemon routes the nudge to the forked lead session that owns the thread (looked up via `topic_sessions`). The fork session has full thread context and decides when to loop in other participants via @mentions — there is no automatic broadcast to all thread participants. The fork should @mention others when a reply contains information they need to act on (e.g., a question directed at them, a decision affecting their work, or context they explicitly requested). Don't @mention for routine updates the fork can handle alone.

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
- `coworker_status_snapshot_ready: bool` — indicates whether a baseline coworker status snapshot has been captured
- `coworker_status_lines: HashMap<String, CoworkerStatusLine>` — memoized status signatures used to detect coworker changes
- `coworker_pulse_frames: HashMap<String, usize>` — per-coworker countdowns for status-change pulse animations
- `spinner_frame: usize` and `spinner_last_tick: Instant` — shared animation clock for lead spinner, channel-lead thinking, and coworker status pulses
- `channel_lead_thinking: HashMap<String, Instant>` — per-topic thinking timers that can keep spinner activity alive

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
- **Task list** (5s): Local filesystem reads (`~/.claude/tasks/`) — nearly instant, no network.
- **PR data** (30s): `prs.status` RPC — GitHub GraphQL, daemon-cached for 60s.
- **Repo status** (60s): Direct `gh` CLI calls for commit/CI/release info.

**Coworker status pulse behavior**:
- Coworker rows in the board side panel are animated only on real status changes.
- `update_coworker_status()` stores a compact per-coworker signature in `coworker_status_lines` and opens a countdown in `coworker_pulse_frames` when any tracked field differs.
- `tick_spinner()` advances `spinner_frame` every `COWORKER_PULSE_INTERVAL` when any spinner-relevant state is active (lead working, in-progress tool activity, or active coworker pulses) and simultaneously advances the wave counters used by `coworker_pulse_wave_step()`.

The split-poll architecture ensures coworker phase changes appear in real-time (2s), task list updates within 5s, while expensive PR data is fetched at a rate that stays within the daemon's 60s cache TTL.

## Web UI

### Asset Resolution

`resolve_web_dir()` (`src/lib.rs`) locates the built web-app static assets at runtime by checking two candidates in order:

1. **`exe_dir/web-app/dist`** — next to the running binary (works for release tarballs, Docker, and curl installs)
2. **`CARGO_MANIFEST_DIR/web-app/dist`** — source tree path baked in at compile time (works for `cargo install --path .` development builds)

The release pipeline builds the web-app once (Node 22, `npm ci && npm run build`) as a platform-independent artifact, then bundles `web-app/dist/` alongside the binary in every platform tarball. The Docker image uses a separate `node:22-slim` build stage and copies the dist into `/usr/local/bin/web-app/dist`. The curl install script extracts and places `web-app/` next to the binary.

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
  - `web-app/src/lib/CoworkerStatus.svelte` filters inactive rows only when phase explicitly indicates idle/done (or status indicates stopped/idle), and keeps rows whose phase is null/empty if `status` indicates active work.
  - This is a defensive fix for mixed payloads where `/status` may omit `phase` but includes `status`.
- Auth profile switching
- Push notifications (W3C Push API with VAPID)
  - **Backend**: `src/push.rs` — VAPID key generation/storage, subscription management, encrypted push delivery via `web-push-native`. Keys stored in `~/.midtown/push/`.
  - **Frontend**: `web-app/src/lib/push.js` — subscribe/unsubscribe flow, VAPID key fetch. `web-app/src/sw.js` — service worker handles incoming push events with foreground suppression (skips OS notification when the app window is focused).
  - **Triggers**: @mentions (`chat.rs`, `rpc_channel.rs`), CI failures on default branch (`mod.rs`), orphaned worktree warnings (`dispatch.rs`), and task completion (`dispatch.rs` via `task_completed_effects()`). All use the `Effect::SendPushNotification` pattern.
  - **Tags**: Each notification uses a unique tag (e.g., `task_completed_{id}`, `mention`, `ci_failure`) to prevent the Web Notifications API from silently replacing unread notifications.
  - **HTTPS requirement**: Mobile browsers require a secure context for `PushManager`. Desktop `localhost` is exempt, but LAN access (`http://192.168.x.x`) will not have push available.
- Responsive layout with three breakpoints:
  - **Mobile (≤768px)**: Tab navigation, hamburger menu with slide-out sidebar, modal popups for task/PR details
  - **Tablet (769–1024px)**: Permanent sidebar replaces tab navigation, two-column grid layout
  - **Desktop (≥1025px)**: Three-column Slack-inspired layout with sidebar, main channel, and toggleable detail panel for tasks, PRs, and coworker info
- Clickable `@coworker` mentions in messages open coworker detail panel on desktop

### Celebration effects

Merged PRs animate across the UI once they land in `kanbanData.done`. `web-app/src/lib/CelebrationEffects.svelte` observes `$kanbanData.done` (only after `$daemonStatus` hydrates to avoid replaying historical merges), keeps a per-session set of celebrated PR keys (`repo#number`), and randomly selects one of ten short-lived CSS effects (confetti, emoji rain, matrix cascade, etc.). Effects render inside a fixed overlay with `pointer-events: none` so they never block interaction, and every particle color comes from `AVENUE_COLORS` or existing theme tokens to stay on brand.

To add a new effect:

1. Create a generator helper within `CelebrationEffects.svelte` that returns the data your effect needs (positions, characters, durations, etc.). Keep payloads serializable (no DOM references) so Svelte can diff cheaply.
2. Append a `{ type, duration, generator }` entry to `EFFECT_DEFS`. Durations should stay under ~5 s and should match the CSS animation timing you introduce.
3. Add a new markup branch inside the `{#each activeEffects}` block plus scoped styles/keyframes for the effect. Reuse the overlay semantics (absolute positioning, opacity fades, `pointer-events: none`) so rapid merges cannot degrade layout performance.
4. Whenever possible, pull palette values from `COLOR_PALETTE`/`AVENUE_COLORS` instead of hardcoding new hex codes. This keeps dark/light themes and future recolors consistent.

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
- **Integration** (`daemon/stream.rs`): `process_universal_events()` accepts the `channel_lead_sessions` map and emits `BroadcastUniversalItems` effects for the main lead (channel=None) and for each active channel lead (channel=Some(channel_name)). Coworker tool calls are not broadcast to the universal events pipeline (they are only visible via DM channels — see below).
- **Broadcast**: The `BroadcastUniversalItems` effect sends `WebUpdate::UniversalItems` to all connected WebSocket clients and updates `DaemonState.recent_tool_items` (a `RwLock<HashMap<String, Vec<UniversalItem>>>`, capped at `MAX_TOOL_ITEMS_PER_AGENT=20` items per agent). Agent name and optional channel are carried at the envelope level (`UniversalItemsData`). The web UI stores items keyed by channel name (`'midtown'` for the main lead, or the topic channel name for channel leads) so each channel view shows only the relevant tool calls.
- **TUI rendering**: The TUI polls `coworkers.status` (at 2s intervals) which calls `collect_tool_activity()` to serialize `recent_tool_items`. The TUI renders a compact activity strip at the bottom of the chat pane showing the most recent tool calls per active agent, using `semantic_header` for tool call labels and "✓ ok" / "✗ error" for tool results.
- **Lifecycle**: Tool activity for a coworker is cleared from `recent_tool_items` when the coworker shuts down (in `shutdown_coworker_impl()`), preventing ghost activity from persisting when the avenue name is reused.

## DM Channel Streaming

Each coworker's text output is streamed to a per-coworker DM channel (`dm-<name>`), mirroring how channel leads stream their output to topic channels. This allows the web UI and TUI to show real-time coworker activity when viewing a specific coworker's DM channel.

**Data flow:**
```
StreamEvent (NDJSON drain) → extract_assistant_text() → aggregated text
    → process_coworker_output() → Effect::PostToChannel { channel: Some("dm-<name>") }
    → channel JSONL file + WebSocket broadcast
```

- **`process_coworker_output()`** (`daemon/stream.rs`): Takes the set of active coworker session names (excluding the main lead, channel leads, and fork-bound sessions) and posts each coworker's aggregated text output to `dm-<name>`.
- **Session separator**: When `SpawnSession` spawns a non-reviewer coworker, a `PostSystemMessage` separator (e.g., "─── Task !42: Fix auth bug ───") is posted to `dm-<name>` to visually delineate session boundaries. Reviewer sessions skip DM separators since they are ephemeral PR-scoped sessions.
- **Reviewer exclusion**: Reviewer sessions do not stream output to DM channels and do not receive DM separators, keeping the DM channel system focused on persistent dev coworkers.

## Workflow Script System

Each channel can have a `workflow.py` script that customizes how the daemon responds to domain events — PR lifecycle, coworker status changes, task transitions, CI results, and more. Scripts are invoked by the daemon via `uv run` using the [Midtown Python SDK](../sdk/python/).

### Script Resolution

`workflow_script_for_channel()` in `src/paths.rs` resolves the active script using a 4-level priority order (first file found wins):

1. `<project_root>/.midtown/channels/<channel>/workflow.py` — channel-specific, committed to repo
2. `~/.midtown/projects/<repo>/channels/<channel>/workflow.py` — channel-specific, local only
3. `<project_root>/.midtown/workflow.py` — project default, committed to repo
4. `~/.midtown/projects/<repo>/workflow.py` — project default, local only

If no script is found, the daemon falls back to its compiled-in default behavior. This layered resolution allows teams to commit shared workflows to the repo while maintaining machine-specific local overrides.

### Invocation

The daemon emits `Effect::EmitWorkflowEvent` at detection points in `pr.rs`, `health.rs`, and `dispatch.rs`. The effect executes the script as:

```
uv run workflow.py --event '{"type":"pr.opened",...}' \
    --state ~/.midtown/projects/<repo>/channels/<channel>/workflow-state.json \
    --socket ~/.local/state/midtown/<repo>/daemon.sock
```

**Changes take effect on the next daemon tick** — no daemon restart required.

### State Persistence

The state file (`workflow-state.json`, path from `workflow_state_file()` in `src/paths.rs`) stores the script's mutable state between invocations. Since workflow scripts are short-lived subprocesses (one `uv run` per event), external persistence is required. The `run()` entry point in the SDK loads state before calling the handler and saves it atomically afterward.

### Python SDK

The Midtown Python SDK (`sdk/python/midtown/`) provides the `run()` entry point and `MidtownRPC` client. A typical workflow script:

```python
from midtown import run, MidtownRPC

def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    if event["type"] == "coworker.idle":
        rpc.post_to_channel(f"{event['coworker']} is idle")

if __name__ == "__main__":
    run(handle)
```

`MidtownRPC` methods: `post_to_channel()`, `spawn_coworker()`, `nudge_coworker()`, `create_task()`, `update_task()`, `complete_task()`, `list_tasks()`, `check_pending()`.

### Reference Implementation

`sdk/python/midtown/default_workflow.py` is the reference implementation that replicates the compiled-in PR lifecycle. Copy it to start customizing:

```bash
mkdir -p .midtown/channels/<channel>
cp $(python -c "import midtown, os; print(os.path.dirname(midtown.__file__))")/default_workflow.py \
   .midtown/channels/<channel>/workflow.py
```

### Event Taxonomy

The `workflow` module (`src/workflow.rs`) defines the `WorkflowEvent` enum:

| Group | Events |
|-------|--------|
| task | `task.created`, `task.assigned`, `task.completed` |
| pr | `pr.opened`, `pr.approved`, `pr.changes_requested`, `pr.merged`, `pr.ci_passed`, `pr.ci_failed`, `pr.conflict` |
| coworker | `coworker.idle`, `coworker.stuck`, `coworker.message` |
| channel | `channel.message` |
| timer | `timer.tick` |

**Serialization contract:** Events are serialized as JSON objects with a `"type"` discriminant (dotted name), a `"channel"` field (always present), and event-specific fields. `task_id` is a `String` matching `Task { id: String }` in `src/tasks.rs`. Optional fields (`task_id` on coworker events, `check_name` on `pr.ci_failed`) are **omitted entirely** when absent — not serialized as `null` — following the `#[serde(skip_serializing_if = "Option::is_none")]` pattern used throughout the codebase. Python scripts should use `event.get("task_id")` to test presence.

**Accessors:** `WorkflowEvent::channel() -> &str` and `WorkflowEvent::task_id() -> Option<&str>` provide typed access without deserializing JSON.

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

## Channel List Changed Events

When a channel is created, archived, unarchived, or renamed, the daemon broadcasts a `WebUpdate::ChannelListChanged` event to all connected WebSocket clients. This allows the web app sidebar to update immediately without polling.

**Broadcast sites** (all mutation paths covered):
- **RPC handlers** (`rpc_channel.rs`): `channel.create`, `channel.archive`, `channel.unarchive`, `channel.rename` — broadcast after successful operation. Create checks `already_exists` to avoid spurious broadcasts from idempotent `Channel::create()`.
- **REST API** (`web.rs`): `POST /api/channels` — broadcast after creation, with the same `already_exists` guard.
- **Effects system** (`effects.rs`): `Effect::CreateChannel`, `Effect::ArchiveChannel`, `Effect::MergeChannels` — broadcast after successful execution. Each has an idempotency guard to prevent duplicate broadcasts from repeated ticks.

**Web app handling** (`api.js`): The `channel_list_changed` WebSocket event triggers `fetchChannels()` to re-fetch the full channel list from the REST API (source of truth), rather than optimistically updating client-side state.

**TUI**: The TUI creates channels via daemon RPC when available (falling back to direct filesystem access if the daemon is unreachable), so the daemon handles broadcasting. The TUI's 30-second channel poll handles updates from other clients.

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
- `recently_recovered_session_ids: HashSet<String>` — Session IDs for which a recovery was recently attempted and succeeded. Pre-evaluated from `state.cooldowns` (category `"session_recovered"`, keyed by session ID) so decision functions stay pure. Used by both `dispatch_via_sessions` (Path 1) and `spawn_for_pending_tasks` (Path 2) to skip re-recovery when a session dies quickly after being recovered, preventing log spam on every 5s tick.

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

## Idle Shutdown Protections

`check_and_shutdown_idle_coworkers()` (in `src/daemon/health.rs`) runs every `SessionMonitorTick` and hands a `rules::IdleShutdownContext` the information it needs to decide which sessions can safely be stopped. The context bundles a number of protection sets built in `collect_world_snapshot()`:

- `busy_coworkers`, `pending_task_owners`, and `coworkers_with_unblocked_deps` keep coworkers alive while they have active work or downstream dependents.
- `coworkers_with_open_prs` stay running until CI passes and review feedback (if any) is handled.
- `active_reviewers` keep their review assignment while `PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS` (30 min) has not elapsed.
- `usage_limited_coworkers`, `api_error_coworkers`, and `auth_error_coworkers` are preserved so recovery flows (limit reset, retry, re-auth) can finish.
- `coworkers_with_active_tools` comes from `ProcessHealth` in-flight markers (`has_pending_tool`, `has_running_subagent`, or `has_pending_api_call`). Tool calls, Task subagents, and fresh pending API turns are treated as critical sections — shutting down mid-turn would drop the result. `has_pending_api_call` is freshness-bounded (uses `last_event_at`/startup time) so stale sessions are still eligible for cleanup.

Only coworkers that fall outside all of these protection sets, are older than `MINIMUM_COWORKER_LIFETIME` (90 seconds — increased from 60s because session startup takes 40-60s, and a 60s guard could expire before initialization completes), and are not the lead session (named after the repo) are eligible for idle shutdown.

## Self-Update (`midtown update`)

`src/bin/midtown/cli/update.rs` implements self-update via GitHub releases. Two entry points:

- **`handle_update(check_only)`** — Interactive update triggered by `midtown update`. Downloads the platform-specific tarball to a `tempfile::TempDir` (RAII cleanup on all error paths), extracts it, then replaces the binary and web-app directory. The web-app swap uses atomic `fs::rename()` (matching `install.sh`) rather than recursive copy, so a crash mid-update never leaves a partial `web-app/` directory.
- **`check_for_update_notice()`** — Non-blocking check called during `midtown start`. Spawns a background thread and uses `mpsc::recv_timeout(3s)` to enforce a wall-clock deadline independent of the 10s HTTP timeout. Writes the last-check timestamp on both success and failure/timeout to preserve the 1-hour rate limit even when GitHub is unreachable.

**Version comparison** (`is_newer`): Strips pre-release suffixes (e.g., `0.7.0-beta.1` → `0.7.0`) before comparing, so pre-release tags are never considered newer than their stable counterpart.
