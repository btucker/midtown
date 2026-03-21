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

Functions in `rules.rs` take immutable data and return decisions. No mutation, no I/O, no async. Phase transitions are returned as data, applied by the caller. If a decision depends on a side effect (spawn success, API call), split into two decisions with an effect in between. The `evaluate_tick()` → `Vec<Effect>` → `execute_effects()` pipeline is the canonical path. Long-running cleanup effects (e.g., `CleanStaleBranches`, worktree directory removal) execute as fire-and-forget `tokio::spawn` tasks so they don't block the `select!` loop. State mutations (registry updates, cooldown recording) always happen synchronously before the spawn; the background task only performs filesystem/git operations.

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

`SessionMonitorTick` (coworker health), `TaskDispatchTick` (work assignment), `NoteReviewTick` (note staleness review). Name components for what they do, not their historical origin.

## Key Patterns

**Effect-based side effects**: Never perform I/O in decision functions. Return `Effect` variants from `rules.rs`, execute them in `effects.rs`. This keeps the core logic pure and testable.

**Temp-file pattern for shell arguments**: When passing long text to the `claude` CLI (system prompts, initial prompts), write to a temp file and use `$(cat file)` in the command string. This avoids shell quoting issues. See prompt writing in `launch.rs`.

**Hybrid process model**: The Project Lead can run headless (daemon-managed) or interactively (`midtown agent attach`) with direct provider exec in the current terminal; coworkers run as headless sessions by default. Interactive attach no longer uses PTY wrapping.

**Centralized path resolution**: All `~/.midtown/` paths derive from `midtown_base_dir()` and its helpers in `src/paths.rs`. In tests, `let _guard = set_test_midtown_base_dir(tmp)` redirects resolution to a temp directory — the guard must be held for the override to remain active.

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

### Headed Sessions (via `midtown start`)

Headed (interactive terminal) sessions are launched via `midtown start`, not `midtown agent`. The `midtown agent` namespace is exclusively for headless daemon-managed sessions. See `midtown start --help` for details.

**Attach/view profile fidelity:** `session.attach` now returns the persisted auth profile from the `SessionRecord`, and all headed attach/view/chat entry points reuse that profile when rebuilding the interactive shell command. This keeps attach flows on the same credentials and `CODEX_HOME`/`CLAUDE_CONFIG_DIR` that the headless session was launched with instead of whatever profile happens to be active locally. Codex prelaunch skill sync also targets that explicit profile directory.

### Daemon-Bypass CLI Commands

Some CLI subcommands bypass the daemon RPC entirely, communicating directly with the webhook HTTP API or performing local-only work:

- `midtown agent upload-image <path>` — uploads a local image file to GitHub's CDN via the `uploads.github.com` endpoint and outputs `![alt](URL)` markdown using the returned `user-images.githubusercontent.com` URL. This bridges Playwright MCP screenshot output (local files) to GitHub-embeddable URLs for PR descriptions. No daemon RPC connection is needed. Coworkers use the Playwright MCP tools (`browser_navigate`, `browser_screenshot`, etc.) to capture screenshots interactively, then `upload-image` to get a PR-ready URL.

Legacy screenshot-serving endpoints (`GET /api/screenshots/<filename>` on the per-project daemon, `GET /api/projects/<repo>/screenshots/<filename>` on the shared gateway) are preserved for backward compatibility — old channel messages reference `[Attached: .../screenshots/<file>]` which the frontend rewrites to these endpoints.

These commands are intercepted in `main.rs` *before* the `DaemonClient::connect()` call and return early. Most bypass commands are listed in the consolidated `unreachable!()` catch-all at the end of the daemon-connected match.

### HeadlessSession I/O Architecture

`HeadlessSession` (`src/headless.rs`) manages the child process and exposes a typed event stream. Claude sessions use a **background reader** pattern to avoid OS pipe-buffer stalls:

- On spawn, two `tokio::spawn` tasks are created — `claude_stdout_reader_loop` and `claude_stderr_reader_loop` — each owning a `BufReader` over the child's piped stdout/stderr.
- Each task continuously calls `read_line()` and forwards parsed events or raw lines into **unbounded `mpsc` channels** (`stdout_rx` / `stderr_rx` stored in `HeadlessSession`).
- `next_claude_event()` does a single `rx.recv().await` — a simple channel receive. Blank lines and `StreamEvent::Unknown` events are filtered in the reader task, not in the hot path.
- `drain_stderr()` waits up to 10ms for any line the background task is mid-read, then drains up to 100 lines non-blocking.
- **Detach-on-drop**: When `detach_on_drop` is set (daemon restart path), the `Drop` impl spawns drain tasks to keep the channel receivers alive. Without this, dropping the receivers would cause the reader tasks to exit, closing the pipe FDs and sending SIGPIPE to the child.

This mirrors the Codex session pattern (`read_stdout_loop` / `read_stderr_loop` in `CodexSharedRuntime`) and ensures the child process never blocks on a full 64 KB kernel pipe buffer regardless of output volume.

**Codex runtime reuse:** In `src/headless.rs`, `CODEX_RUNTIME` is a `HashMap` keyed by profile (`codex-runtime|<CODEX_HOME>`), not a single global process. The daemon now shares one app-server per profile and reuses it across sessions, while isolating runtime state between profiles. `HeadlessSession::spawn()` derives the profile directory from provider env (`CODEX_HOME`) before running prelaunch hooks so skill sync lands in the same profile that will back the runtime key.

### Session-Centric Model

The daemon uses a **session-centric model** where Claude Code sessions (keyed by session ID) are the primary coordination entity. Names are ephemeral labels drawn from an LRU pool.

**NamePool** (`src/name_pool.rs`): Manhattan avenue names (lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon) are managed in an LRU queue. When a session spawns, it allocates a name from the front of the queue. When it shuts down, the name returns to the back. Preferred name hints allow a resumed session to get its previous name when available, preserving branch and worktree continuity. **Name exclusion**: All allocation call sites (`dispatch.rs`, `pr.rs`, `rpc_coworker.rs`) exclude both channel lead names and `active_names` (from `WorldSnapshot` or `SessionManager`) to prevent collisions when a session is still running but its coworker was cleaned up from `CoworkerManager`.

**SessionRecord** (`src/daemon/state.rs`): Each session is tracked by a `SessionRecord` containing session ID, task ID, current and preferred names, worktree path, branch, PR number, and running state. Records persist across daemon restarts in `persistent_state.json`.

**Resume fallback path** (`spawn_with_resume_fallback` in `src/daemon/effects.rs`): Resume effects first attempt the requested resume mode and automatically fall back to a fresh spawn if resume fails. The fallback injects a handoff prompt with the prior context so the new session can continue the work with minimal context loss instead of dropping the request.

**Dispatch** (`src/daemon/dispatch.rs`): Two dispatch paths handle session recovery. **Path 1** (`dispatch_via_sessions_with_task_lookup`, called from `dispatch_via_sessions()`) examines in-progress tasks with session records. For stopped sessions, it emits `SpawnSession` effects with `resume=true` and the session's preferred name, unless the coworker is an active reviewer or the session was recently recovered (per-session cooldown prevents re-recovery spam). This replaces the legacy orphan-recovery pattern with a unified session-aware dispatch path.

**Path 2** (`spawn_for_pending_tasks_excluding`, `src/daemon/dispatch.rs`): Handles pending tasks whose stopped session has no active task assignment. Before building the `LaunchConfig`, it validates the recorded `working_dir` with `.exists()`: if the worktree has been cleaned up since the session last ran, Path 2 falls back to a freshly computed worktree path (from `prepare_task_worktree`) and logs a warning. The chosen path is passed as `working_dir` in the `SpawnSession` effect. On successful spawn, the `SpawnSession` handler in `effects.rs` updates `record.working_dir` with the actual path used — ensuring the stale path never persists into the next tick and preventing repeated fallback log spam.

**Dispatch task limit** (`max_in_progress_tasks`): Replaces the former process-based `max_coworkers` limit. The limit is task-count-based: dispatch stops when `in_progress_tasks.len() + spawns_queued_this_tick >= max_in_progress_tasks`. All task types (dev, reviewer, ops, specialized) share the same limit — there is no separate `REVIEW_HEADROOM` or dev-vs-reviewer distinction. The `spawns_queued_this_tick` counter prevents overshooting within a single tick. `snap.is_at_task_limit` is pre-computed for pure decision functions; `DaemonState::is_at_task_limit()` reads from disk for RPC handlers outside the snapshot pipeline.

**Dispatch priority** (`src/daemon/dispatch_priority.rs`): `prioritize_pending_tasks()` orders pending tasks before the dispatch loop iterates them. Three tiers (stable sort — FIFO within each tier):
1. **Children of in-progress parents** — `task_parent_map[task_id]` exists and the parent is currently in-progress.
2. **Blockers** — the task blocks at least one other task (appears in the inverted `blocks_map`).
3. **FIFO** — everything else, ordered by creation time.

The `blocks_map` (`HashMap<String, Vec<String>>`) is the inverse of `Task.blocked_by`: for each task X that appears in some task Y's `blocked_by`, map X → [Y, ...]. It is computed during snapshot collection and added to `WorldSnapshot`. Only pending/in-progress tasks participate — completed tasks are excluded.

**In-memory reverse maps** on `DaemonState`:
- `name_to_session` / `session_to_name` — bidirectional name↔session lookup
- `task_to_session` — task→session mapping for dispatch decisions

**Task assignment persistence**: `sessions[].task_id` on `SessionRecord` is the single source of truth for coworker→task mapping. `RecordTaskAssignment` (emitted by both `SpawnSession` and `NudgeSessionWithCallbacks` callback chains) updates two things: (1) `sessions[].task_id` in `DaemonPersistentState` so that insight routing, busy tracking, and other session-aware features can resolve the task's channel, and (2) the `task_to_session` reverse map for dispatch lookups. The `WorldSnapshot::session_task_map` field (task_id → session_id) is built from session records during `collect_world_snapshot()`, enabling O(1) lookup of "which session is working on task X?" via `WorldSnapshot::find_session_for_task()`. The `WorldSnapshot::name_task_assignments` field is also derived from session records, ensuring dispatch decisions always reflect the persisted state. The `WorldSnapshot::busy_coworkers` field is derived from sessions cross-referenced with in-progress tasks (not from task file owners).

**Parent-child task grouping**: `DaemonPersistentState::task_parent` (`HashMap<String, String>`) maps child task ID → parent task ID. This is a UI-level grouping relationship for showing related tasks (e.g., a review follow-up task as a child of its implementation task). Child tasks can start while the parent is open — this is purely organizational, not a blocking dependency. The web UI renders parent-child relationships in the kanban board. Orphaned entries are pruned by the state garbage collector (`GarbageCollectState`).

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

### Agent Definitions (`src/agent_definition.rs`)

The `agent spawn` command supports `--agent <name>` to load agent definitions from markdown files with YAML frontmatter, following Claude Code's agent definition format:

- **Search paths** (in order): `.claude/agents/{name}.md` (project-level), `~/.claude/agents/{name}.md` (user-level)
- **Frontmatter fields**: `name`, `description`, `model` (all optional — `name` falls back to filename)
- **Body**: Used as the agent's system prompt, prepended to the coworker's initial prompt under `## Agent Instructions`
- **Model override**: If the agent specifies `model`, it overrides the default provider-resolved model (subject to downstream normalization by `normalize_model_for_provider_role`)

Additional call-in flags: `--channel <name>` sets `LaunchConfig.channel` for message routing; `--thread <id>` registers a `fork_bound_threads` binding so the coworker's posts auto-route to the specified thread; `--task <id>` assigns the coworker to a task with full lifecycle management (worktree creation, task ownership, plan/execution-skill context in prompt, disk state updates). The `--task` flag replicates the same assignment flow that the daemon's automatic dispatch uses: it validates the task is pending, creates/reuses a task worktree, sets the task to `in_progress` with the coworker as owner, and reads plan/execution-skill metadata from persistent state via `build_plan_prompt_section_from_parts`. Channel is auto-resolved from the task if `--channel` is not explicitly provided.

## Prompt Architecture

Prompts are assembled from three distinct layers in `src/agents.rs`:

1. **Agent definition (Layer 1)** — Role identity and behavioral instructions, loaded from `.claude/agents/midtown-*.md` (Claude Code agent format with YAML frontmatter). Search order: `.claude/agents/` (project-level), `~/.claude/agents/` (user-level), then compiled-in fallback. Wired into session spawning via `HeadlessConfig.agent_name`. For Claude Code sessions, Layer 1 is delivered via the `--agent <name>` CLI flag (e.g., `--agent midtown-code-author`) on both fresh and resume sessions, while Layers 2+3 go through `--append-system-prompt` on fresh sessions only. For Codex sessions (which don't support `--agent`), all layers are bundled into `--system-prompt` as before. The mapping from `CoworkerRole` to agent name is in `CoworkerRole::agent_name()`, and `render_append_prompt()` returns only Layers 2+3.

2. **Shared prompt (Layer 2)** — Operational rules shared across roles, loaded via `shared_prompt_for_role()`. Uses compiled-in content from `agents/common.md` and `agents/lead-common.md`. Coworkers/reviewers get `common.md` only; leads get `lead-common.md` + `common.md`.

3. **Runtime context (Layer 3)** — Template variable replacement and runtime content injection, applied via `build_runtime_context()`. Ops extras (`agents/ops-channel-lead.md`) are appended before substitution (so `{name}` IS replaced in ops content). AGENTS.md is appended after substitution (so literal `{name}` in AGENTS.md is preserved).

**Assembly by agent type:**
- **Project Lead**: `midtown-project-lead.md` (Layer 1) + `lead-common.md` + `common.md` (Layer 2)
- **Coworker**: `midtown-code-author.md` (Layer 1) + `common.md` (Layer 2)
- **Reviewer**: `midtown-code-reviewer.md` (Layer 1) + `common.md` (Layer 2)
- **Channel lead**: `midtown-channel-lead.md` (Layer 1) + `lead-common.md` + `common.md` (Layer 2) + `ops-channel-lead.md` for ops (Layer 3)

**Template variables** (in Layer 2/3 content): `{name}` (agent name; project name for Project Lead), `{project_name}` (e.g., `midtown`), `{channel_lead}`, `{escalation_target}`, `{channel_name}`, `{domain_context}`, `{code_review_invocation}`.

**@mention routing:** Agents use `@{project_name}` (e.g., `@midtown`) to mention the Project Lead — not the literal `@lead`. Both `@lead` and `@{project_name}` are recognized by the daemon's nudge routing in `rpc_channel.rs` and `chat.rs`.

**Task-based @mention routing:** When a lead @mentions a coworker and includes a task ID (`!N`), the daemon's `route_mentions()` in `chat.rs` resolves the actual session to nudge by looking up the task owner from the task system (`crate::tasks::get_in_progress_tasks_with_subjects_for_repo`). If the resolved owner is running, the nudge is routed to them instead of the @mentioned name — this ensures feedback reaches the correct session even if coworker names have been reassigned. Example: `@park !42 here's your review feedback` routes to whoever is working on task 42. Falls back to name-based routing when the task ID is not found or the owner is not running. **Note:** `Coworker.current_task` is a display-only field (always `None` in storage, populated dynamically for API responses); the task system file store is the authoritative source for task ownership.

## Project Identity: `dir_key` vs `project_name`

Two distinct identifiers travel through the codebase:

| Concept | Example | Used for |
|---------|---------|----------|
| `dir_key` | `"midtown.nosync"` | Filesystem paths (`~/.midtown/projects/<dir_key>/`), config file lookup, task storage |
| `project_name` | `"midtown"` | Channel names, session names, team names, lead identity checks, display |

These are carried together in `ProjectPaths` (`src/paths.rs`), which consolidates ~20 path functions and both identifiers:

```rust
pub struct ProjectPaths { dir_key, project_name, base, state_base }
```

**Resolution**: `ProjectPaths::new(dir_key)` reads `[project].name` from `config.toml`. If not set, auto-derives by replacing dots with hyphens (`midtown.nosync` → `midtown-nosync`).

**DaemonState** carries `paths: ProjectPaths` (for filesystem operations) and `project_name: String` (for identity). The old `repo_name` field is removed.

**WorldSnapshot** carries both `dir_key` and `project_name` as separate fields. Decision functions use `snap.dir_key` for path/config/task operations and `snap.project_name` for identity checks. Old JSON fixtures with `repo_name` are supported via `#[serde(alias = "repo_name")]` on `dir_key`.

**Effect enum** fields that carry the filesystem key are named `dir_key` (e.g., `CompleteTask { task_id, dir_key }`, `AssignAndSpawn { ..., dir_key, ... }`).

## Main Lead Session Identity

The main lead session name equals the project name (e.g. `"midtown"`), not the hardcoded string `"lead"`. This applies everywhere:

- **Spawn**: `LaunchConfig::lead()` sets `name` from the `dir_key` param (`src/launch.rs`)
- **Health**: `ensure_lead_alive()` and `maybe_refresh_lead_session()` compare against `snap.project_name` (`src/daemon/health.rs`)
- **Dispatch**: coworker-limit checks use `is_project_lead()` (`src/daemon/dispatch.rs`)
- **Effects**: auto-detach suffix check uses `state.project_name` (`src/daemon/effects.rs`)
- **Stop-time key**: `coworker_stop_times` entries for the lead are keyed by `project_name.to_lowercase()`
- **Attached key**: `attached_coworkers` entries for the lead are keyed by `project_name` (lowercase)

All lead-identity checks use `helpers::is_project_lead(name, project_name)` (`src/daemon/helpers.rs`), which accepts both the canonical project name and the legacy `"lead"` string for backward compatibility with older sessions. This single helper is used by `rpc_coworker.rs`, `rpc_status.rs`, `dispatch.rs`, `mod.rs`, and `pr.rs` to ensure consistent behavior. The `coworkers.status` API (in `rpc_coworker.rs`) uses `is_lead_health_active(health, project_name)` to check both health-map keys. Per-channel-lead activity is computed by `build_channel_leads_working()`, which maps each registered channel lead name to a bool via `is_session_actively_working()`.

**Backward-compat guard — `is_project_lead()`**: In `helpers.rs`, `is_project_lead(name, project_name)` encapsulates the two-condition check for lead sessions: it returns `true` if the name equals the project name *or* the legacy literal `"lead"` (case-insensitive). All code that needs to identify or exclude the lead session should call this helper rather than inlining the two-condition check. Current callers: `handle_coworker_list()`, `handle_coworkers_status()`, `handle_coworker_report_state()` (all in `rpc_coworker.rs`), and `handle_status()` (`rpc_status.rs`).

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

**Agent definition model overrides:** When `agent spawn --agent <name>` specifies an agent definition with a `model` field, the call-in handler (`rpc_coworker.rs`) resolves the auth_provider from the model alias via `provider_for_model_alias()` before building the `LaunchConfig`. This ensures the model and provider are consistent — without it, `spawn_coworker()` would silently normalize the model to match the caller's provider via `normalize_model_for_provider_role()`, defeating the agent definition's model intent.

**Agent definitions** (`src/agent_definition.rs`): Markdown files with YAML frontmatter (name, description, model) and a system prompt body. Searched in `.claude/agents/{name}.md` (project-level) then `~/.claude/agents/{name}.md` (user-level). The parsed `AgentDefinition.model` feeds into provider resolution, and the body becomes the coworker's system prompt.

## Channel Leads

Channel leads are headless Claude Code sessions attached to individual topic channels. They are on-demand domain experts — spawned when triggered, shut down when idle, and resumed within a daemon run.

**Role:** A channel lead brainstorms, maintains living design documents, answers domain questions, and tracks awareness of active tasks and PRs in its channel. It does not write code, open PRs, or create tasks. When implementation work is needed, it escalates to `@{project_name}`.

**Lifecycle:** Channel leads are spawned on-demand by these triggers:
- **User message** in the channel (via `handle_channel_post`)
- **Task created** in the channel (via `handle_task_create`)
- **Insight posted** to the channel (via `post_insight` in `effects.rs`)
- **Explicit nudge** (@mention routing, task feedback)

**Archived-channel redirect:** When a task is created with `--channel <name>` pointing to an archived channel (e.g., `daemon`), `handle_task_create` redirects the task to the ops channel via `resolve_effective_task_channel()`. The effective channel is stored in both the task JSON and `ps.task_channel`, ensuring all downstream routing (announcement posting, `NudgeChannelLead`, `MIDTOWN_CHANNEL` injection, insight posting, `handle_task_metadata`) uses the routable channel. If the ops channel is itself archived, the redirect falls back to the main channel.

All triggers use the `NudgeChannelLead { channel_name, reason }` effect. The execution layer in `effects.rs` routes with session-id-first behavior to avoid name collisions: it tries `send_message_to_session_id()` using the stored channel mapping; if stale/missing, it refreshes from the active named session; if resume is possible, it uses `spawn_with_resume_fallback(...)` and then sends to the resumed/fresh session; otherwise it spawns fresh and persists the new session ID.

The project lead is the channel lead for the main channel — `NudgeChannelLead` routes to the project lead's dual-path nudge (headless session manager or headed intercom) when the channel is the default channel.

Channel leads participate in normal idle shutdown (same timeout as coworkers). The `channel_lead_sessions` map is rebuilt at startup from session records and then maintained during runtime; `WakeReason` (in `src/daemon/wake_reason.rs`) captures why a session is being woken and provides formatting for both nudge messages and initial prompts. Typed variants (`TaskAssigned`, `TaskClaimed`, `SessionRecovery`, `ReviewAssigned`) carry structured data and generate rich messages (e.g., `ReviewAssigned` loads the `agents/reviewer-resume.md` template (a brief resume that references the system prompt and includes the code-review skill invocation)); the generic `Nudge` variant wraps freeform strings for health alerts and ops notifications. `UserMessage` and `Mention` both carry an optional `ThreadContext` (parent ID + channel name) so that nudge recipients receive `--thread`/`--channel` reply instructions when the triggering message is a thread reply.

**Nudge-to-DM-thread routing:** When `PostToChannel` carries a `nudge_type` (set by `WakeReason::to_nudge_type()`), the message is written to the coworker's DM channel (`dm-<name>`) with `MessageType::Nudge` and the `nudge_type` field preserved. Root leads are excluded from DM mirroring because they already own a native channel surface; only agents without a home channel (coworkers, reviewers, and currently forks) use DM mirrors. The RPC layer (`rpc_channel.rs`) includes `nudge_type` in channel history responses, and `WebUpdate::ChannelMessage` broadcasts it to WebSocket clients so the web UI can render nudge-specific styling. The `MessageType::wire_name()` method provides stable wire-protocol strings for all message types (explicit match, not `Debug` format).

Note: `route_mentions()` is enabled for non-user, non-system senders in topic channels (e.g., channel leads and coworkers). User `@coworker` and `@all` mentions in topic channels are still silently dropped — only agent-to-agent mentions are routed. Protected senders (`SKIP_SENDERS`: "midtown", "system", "github", "user") are excluded, consistent with the chat monitor guard.

**System prompt:** Channel leads use the three-layer architecture via `channel_lead_system_prompt()` in `src/agents.rs`: Layer 1 (`channel-lead.md`) + Layer 2 (`lead-common.md` + `common.md`) + Layer 3 (ops extras, template substitution, AGENTS.md). Ops extras are appended and substituted in Layer 3; `AGENTS.md` and `SKILL.md` plugin bodies are appended *after* substitution to preserve any literal placeholder-like text in injected content. The `CoworkerRole::ChannelLead` variant carries `agents_md: Option<String>` and `skill_bodies: Vec<(String, String)>` alongside the existing `channel_name` and `domain_context` fields.

**Plugin discovery for channel leads:** At spawn time (in `effects.rs` and `rpc_auth.rs`), three items are loaded via `spawn_blocking`: (1) channel notes via `load_channel_notes()`, (2) `AGENTS.md` content via `agents_md_for_channel()` in `src/paths.rs` (searches channel-specific then project-wide paths, both in-repo and local), and (3) `SKILL.md` bodies via `collect_skill_md_bodies()` in `src/paths.rs` (strips YAML frontmatter, extracts name and markdown body from each plugin's `SKILL.md`). All three discovery functions are also called in the `handle_auth_switch` path to ensure channel leads retain plugin context across auth profile rotations.

**Domain context from notes:** The `{domain_context}` variable is populated by `load_channel_notes()` in `src/channel.rs`, which reads all `.md` files from `channels/<name>/notes/`, concatenates them with filename-derived headers, and caps total size at 100 KB. This is called at all 4 channel lead spawn sites (3 in `effects.rs`, 1 in `cli/lead.rs`) so that channel leads always start with their accumulated domain knowledge. Insight nudges include a reminder to save important knowledge to notes, completing the feedback loop.

**Note staleness review:** `NoteReviewTick` fires hourly (`NOTE_REVIEW_CHECK_INTERVAL`). In `run_tick()`, the snapshot is enriched with `stale_channel_notes` (via `find_stale_notes()` in `channel.rs`) only for this tick — not on the hot path. The pure decision function `check_for_stale_notes()` in `health.rs` reads `snap.stale_channel_notes` and `snap.note_staleness_cooldown_channels` to emit `NudgeChannelLead` + `RecordCooldown` effects for channels with stale notes (reviewed_at > 72h or missing). Cooldown is 24h per channel (`NOTE_STALENESS_NUDGE_COOLDOWN_SECS`). Notes in archived or DM channels are skipped. CLI: `midtown notes review <path>` stamps `reviewed_at` in YAML frontmatter; `midtown notes list [--channel] [--stale]` lists notes with staleness status.

**Worktree freshness:** `collect_stale_channel_lead_worktrees()` in `snapshot.rs` runs `git fetch origin <default_branch>` (with a 15s timeout) in the project root, then uses `git merge-base --is-ancestor origin/<branch> HEAD` (5s timeout) per channel lead worktree to detect those that are behind. Results are cached in `DaemonState.worktree_freshness_cache` for 25s to avoid repeated git fetches across tick types. The pure decision function `check_channel_lead_worktree_freshness()` in `health.rs` reads `snap.stale_channel_lead_worktrees` and emits `NudgeChannelLead` + `RecordCooldown` effects. Cooldown is 10 minutes per channel (`LEAD_WORKTREE_FRESHNESS_COOLDOWN_SECS`). `WorldSnapshot` fields: `stale_channel_lead_worktrees: HashSet<String>`, `lead_worktree_freshness_cooldown_channels: HashSet<String>`, `default_branch: String`.

**Hard tool restrictions:** Channel leads have code-modification tools (`Write`, `NotebookEdit`) blocked at the CLI level via `--disallowedTools`, enforced by `channel_lead_disallowed_tools()` in `src/launch.rs`. This is a hard enforcement mechanism that the LLM cannot bypass — the Claude Code CLI rejects these tool calls before execution. `Edit` is intentionally *not* blocked because channel leads need it to maintain their notes and workflow files in `~/.midtown/projects/*/channels/*/`. `Bash` is intentionally *not* blocked because channel leads need it for coordination commands (`midtown task create`, `midtown channel post`, etc.). The soft system prompt instruction in `channel-lead.md` ("Do NOT use Write, NotebookEdit, or Bash to modify code") remains as behavioral guidance, and Edit is restricted to notes/workflow files only. When channel leads fork into thread-specific sessions, the fork uses a stricter `disallowed_tools` list via `channel_lead_fork_disallowed_tools()` that re-adds `Edit` to the hard-block list. Fork sessions have narrower context (scoped to a single thread) and are historically more prone to ignoring prompt-based restrictions (see PR #1667), so Edit is only allowed for the top-level channel lead session. This is enforced in `create_fork_session_config()` in `rpc_session.rs` (conditional on the parent being a channel lead). Non-channel-lead forks (e.g., coworker forks) receive no tool restrictions. The `HeadlessConfig.disallowed_tools` field carries the restriction list through `build_claude_headless_args()` in `platform.rs`. Note: Codex does not support `disallowed_tools`, so when the provider is Codex, the hard tool restrictions are skipped and enforcement relies solely on the prompt-based instruction in `channel-lead.md`.

**Coworker guidance:** Coworkers are instructed to `@{channel-name}` (e.g., `@daemon-core`) for domain questions and to reserve `@{project_name}` for coordination, task, and priority questions.

### Forked Sessions (Thread-Specific Channel Leads)

Channel leads can fork themselves into thread-specific sessions via the `session.fork` RPC (`midtown agent fork --thread-id <id>`). A forked session gets an independent session ID bound to a specific thread. Claude/z.ai forks launch as fresh sessions (headless sessions don't persist JSONL files, so `--fork-session` has nothing to fork from); Codex uses `thread/fork`. Fork context is injected via the initial nudge message rather than inherited from the parent session.

**Root session as router:** The root session stays lightweight — it handles top-level messages and decides when to fork. Once a fork exists for a thread, subsequent replies in that thread bypass the root session entirely and route directly to the fork.

**User-controlled forking (web UI):** Topic channel threads are handled by the channel lead by default. Users can explicitly create a dedicated session for a thread via the "Dedicate session" button in the thread panel header. This triggers the `session.fork_thread` RPC (via WebSocket → `ForkThread` client message), which calls `create_fork_session` to spawn a fork bound to the thread. Users can return the thread to the channel lead via "Return to main" (`session.unfork_thread` → `ShutdownSession`). The web UI shows fork status via an avatar indicator dot and the toggle button state.

**`create_fork_session` helper:** A `pub(crate)` shared helper in `rpc_session.rs`, callable from `handle_session_fork` (explicit CLI call), `handle_session_fork_thread` (web UI fork), and the channel lead's own `session.fork` RPC. The `channel_hint` parameter supplies the known channel name even when session records are stale. Channel resolution follows a priority chain: `channel_hint` → `session.channel` → `state.repo_name` (main channel fallback). The repo-name fallback replaced an earlier `caller_name` fallback that produced ghost channels when non-channel-lead callers forked.

**Initial nudge on fork:** Both the CLI and web-UI fork paths send a `NudgeSession` to fresh forks so they have an initial message to act on (without a nudge, forks sit idle forever). The CLI path (`handle_session_fork`) follows a 3-priority fallback chain: (1) an explicit `initial_message` parameter is always used when provided; (2) otherwise, the daemon looks up the parent message by `thread_parent_id` from the channel history and includes its content — for channel leads this is combined with `fork_initial_framing`, for non-channel-lead callers (e.g. the project lead) the message is wrapped as investigative context; (3) if no parent message is found, channel leads get bare `fork_initial_framing` while non-channel-lead callers get no nudge (the framing text assumes a channel-lead role which would be misleading).

**Lead self-fork for deep work:** The project lead (main channel) uses `midtown agent fork --thread-id <id>` to handle multi-turn research (code exploration, debugging investigation, task scoping) without blocking the main channel. The project lead decides when to fork based on message complexity. The lead does NOT fork for: quick one-turn answers, simple task creation, status checks, or forwarding user suggestions to coworkers. Only multi-turn work that would block the root session for more than ~30 seconds triggers a fork. This is a behavioral pattern guided by agent instructions (`lead-common.md`), not daemon automation.

**Thread routing priority:** When a message arrives with `thread_parent_id` set, `handle_channel_post` checks `topic_sessions[thread_parent_id]` first. "pending" entries are filtered out (a concurrent fork is in progress but not yet ready) — the reply falls back to `NudgeChannelLead` rather than producing a nudge with an invalid session ID. Once the fork completes, subsequent replies route to the real fork session. New top-level messages always go to the channel lead. **Important:** routing a reply to the fork does **not** automatically notify other thread participants — if the fork wants a coworker or reviewer to see a thread reply, it **must @mention them** explicitly.

**`session fork` during fork spawn window:** If a `session.fork` call arrives while another fork is still spawning for the same thread (the "pending" sentinel is set), the handler returns `{pending: true, thread_parent_id: "..."}` instead of an error, so the caller can distinguish "retry shortly" from a hard spawn failure.

**Thread ownership broadcast:** When a fork is created or destroyed (including fork process death), the daemon broadcasts a `ThreadOwnership` `WebUpdate` to all WebSocket clients. The `ThreadOwnershipData` struct contains: `thread_parent_id`, `channel`, `has_dedicated_session` (bool), `owner` (fork session's agent name, e.g., "web-discuss-ab12"), and `parent_lead` (the channel lead's display name, resolved via `channel_lead_sessions`). The frontend stores these in three Svelte stores: `threadOwnership` (boolean flag), `threadForkOwners` (fork session name for activity dot coloring), and `threadForkParents` (parent lead name for displaying fork messages with the lead's name/color instead of "fork-XXXX"). Both the CLI fork path (`handle_session_fork`) and the web-UI fork path (`handle_session_fork_thread`) broadcast this update for fresh forks.

**Data flow:**
- `topic_sessions` (in-memory `Mutex<HashMap<String, String>>`) maps `thread_parent_id → fork_session_id`. Entries are "pending" (spawn in progress) or a real session ID. Used by both manual fork and thread-reply routing in `handle_channel_post`.
- `fork_bound_threads` (in-memory `Mutex<HashMap<String, String>>`) maps `fork_name → thread_parent_id`. Used by the output binding path in `handle_channel_post` to auto-tag forked session posts with their bound thread (avoids the async `persistent_state` lock on the hot path).
- `DaemonPersistentState.task_thread_id` maps `task_id → thread_parent_id`. Populated in two ways: (1) explicitly via `--thread-id` on `midtown task create` (the CLI defaults to `$MIDTOWN_BOUND_THREAD_ID` inside fork sessions), or (2) automatically defaulted to the task's announcement message ID when no explicit thread ID is provided. This ensures every task's coworker posts route to the task announcement thread by default. `SpawnSession` reads this mapping to set `bound_thread_id` on the spawned coworker's `SessionRecord`.
- `SessionRecord.bound_thread_id` (persisted) stores the binding on each session so restarts can rebuild the cache.
- `name_to_session` / `session_to_name` reverse maps are backfilled in `create_fork_session`. Although fork sessions now launch as fresh sessions (which do emit `system/init`), the backfill remains necessary because the session ID is pre-assigned via `--session-id` and the `create_fork_session` caller needs these mappings immediately — before the init event arrives asynchronously.

**Auth profile resolution:** Fork sessions must use `active_profile_dir_for_project_with_provider(repo_name, provider)` (project-aware) to resolve credentials — never `current_profile_dir_for(provider)` (global-only). The project-aware path checks the project config's per-provider profile mapping first, then falls back to the global marker. After a per-project `auth switch`, only the project config is updated; the global marker is unchanged. Using the global-only path would give forks stale pre-switch credentials. This is handled by `build_fork_config()` in `rpc_session.rs`, which matches the coworker relaunch path in `rpc_auth.rs`.

**Architectural invariants:**
- Fork sessions are NOT registered in `CoworkerManager`. They bypass `spawn_coworker()` entirely, which means they are excluded from idle-shutdown evaluation and coworker status tracking.
- Thread-to-session routing uses `SessionRecord.bound_thread_id` via `session_by_thread()`. This is the single source of truth for which session owns a thread.

**Dead fork behavior:** Dead forks are not auto-respawned. When a fork process dies, `cleanup_dead_coworker_state` marks the SessionRecord as `is_running: false`. When a thread reply arrives for a dead fork, `handle_channel_post` checks `session_manager.is_alive()` and routes the message to the channel lead instead. Users or leads can create new forks when needed.

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
- `channel.post` — Append a message to a channel; handles `/me` actions, @mention routing, review note deduplication, thread parent ID validation (rejects posts with a `thread_parent_id` that doesn't match any existing message, preventing invisible "black hole" messages)
- `channel.read` — Read messages from a channel (supports `all`, `last`, `since`, `thread`, `message`, `context`, and per-channel filtering)
- `channel.create` — Create a new channel directory; idempotent (no-op if channel already exists)
- `channel.archive` — Rename `channels/<name>/` to `channels/<name>.archived/`; returns an error if the channel does not exist or if archiving the project's main channel
- `channel.unarchive` — Rename `channels/<name>.archived/` back to `channels/<name>/`; returns an error if the channel is not archived or if another active channel of the same name exists
- `channel.rename` — Rename `channels/<old>/` to `channels/<new>/`; updates `task_channel`, `channel_lead_sessions`, and `sessions` in persistent state; shuts down the old channel lead session; returns an error if the old channel does not exist, the new name is invalid/already exists, or if renaming the project's main channel
- `channel.list` — Return all channels, optionally including archived ones

**Channel directory setting**: Each channel can have an optional working directory override stored in `channels/<name>/directory`. When set, channel leads and coworkers spawned for that channel use the subdirectory (relative to the repo root) as their session cwd instead of the worktree root. The setting is read via `read_channel_directory()` and applied to `LaunchConfig.cwd_subdir` at all channel lead spawn paths (initial creation, health-tick respawn, nudge resume/fresh, auth rotation). At spawn time (`spawn_coworker`), `cwd_subdir` is joined to the worktree root; if the resulting path doesn't exist, it falls back to the worktree root with a warning. The web API endpoints `GET/PUT /api/channels/{channel}/directory` manage the setting, with symlink-aware containment validation via `canonicalize()`.

> Note: Channels are no longer auto-archived when all tasks complete. Archiving and unarchiving are explicit user actions via the CLI/RPC methods above.

## Channel Sync

Coworkers stay synchronized via a Claude Code Stop hook. When Claude pauses, the hook reads new channel messages and checks for unclaimed tasks. This means coworkers automatically receive updates at natural pause points.

## Nudge System

Nudge decisions are made in `src/rules.rs` (`decide_interrupt_nudges`, `decide_prompt_nudges`) using `CooldownTracker` for per-coworker cooldowns and `CoworkerPhase` for deduplication (Idle → Prompted → Interrupted). Delivery is via `Effect::NudgeCoworker` / `Effect::NudgeLead` in `src/daemon/effects.rs`.

**CooldownTracker atomicity**: Always use `check_and_record()` rather than separate `check()` then `record()` calls. Separate calls create a TOCTOU window where multiple callers can observe an expired cooldown before either records it, leading to duplicate actions. `check_and_record()` atomically tests and claims the slot via `&mut self`, preventing this race.

- **Project Lead nudges**: Delivered via daemon nudge effects (`Effect::NudgeLead`) and rendered in the active lead session path (headless stream or interactive attach)
- **Coworker nudges**: JSON streaming via `SessionManager` for headless sessions

**Review content embedding**: PR feedback nudges (GreenWithFeedback, ReviewComplete, ChangesRequested, Approved, ReviewComment) embed the full review body inline via `format_review_content()` in `helpers.rs`. This fetches both formal GitHub reviews and Midtown coworker issue-comment reviews (detected by `text_contains_review_signature()`), so the nudged coworker sees all feedback without running extra `gh` commands. On the polling path (`poll_prs_for_issues`), review content is pre-fetched in bulk before decision functions to keep I/O out of the decision phase. Webhook handlers call `fetch_review_content()` directly since they're already event-driven I/O paths.

## PR Merge Gating

When a coworker calls `midtown pr merge --pr <N>`, the daemon runs a pre-gate and three gates before enabling auto-merge:

**Pre-gate — Reviewer active**: Checks `pr_has_active_reviewer(pr_number)` which queries `task_session_spans` for an open span whose task maps to this PR. If a reviewer coworker is assigned to the PR, the merge is rejected immediately — before any API calls. Long-running reviews are never bypassed because the span remains open until the review actually completes or the PR is closed. This is the only gate that cannot be bypassed by prompt-based instructions.

1. **Gate 1 — Review completed**: Checks `is_pr_reviewed()` which looks in the persistent `reviewed_prs` set. A PR is only marked as reviewed when the **assigned reviewer** posts the review — bot comments, unrelated coworkers, and other noise are filtered out via `review_author_matches()` (body-based identity extraction from `<!-- midtown: name -->` frontmatter or review signatures). Formal reviews with strong states (APPROVED / CHANGES_REQUESTED) are accepted even with empty bodies since these are deliberate human actions. The `WebhookEvent.review_author` field carries the extracted identity for the webhook path; the polling path calls `active_reviewer_for_pr(pr_number)` on `DaemonPersistentState` to find the current reviewer from open `task_session_spans`. When no reviewer is assigned, any valid review is accepted (backward-compatible).

2. **Gate 2 — CI passing**: Checks `statusCheckRollup` from `gh pr view` and also verifies `reviewDecision != "CHANGES_REQUESTED"`. The PR must be in `OPEN` state (merged/closed PRs are rejected before gate checks).

3. **Gate 3 — Feedback addressed**: For each review comment ID in `pr_review_comment_ids`, checks that a subsequent PR comment contains `<!-- addresses-review: {id} -->`. Unaddressed feedback blocks the merge.

**Review comment ID collection**: Review comment database IDs are collected via two paths:
- **Webhook path** (primary): When `handle_issue_comment` or `handle_review_comment` detects a code review (via `is_review_comment()`), the `WebhookEvent.review_comment_id` field carries the comment's database ID. Both handlers also process `"edited"` events to catch the placeholder-then-edit pattern (post a placeholder, then edit it with the final review). Edits to already-posted reviews (e.g. typo fixes) are ignored. The daemon handler persists review comment IDs via `add_review_comment_id()`.
- **Polling fallback**: When `is_pr_reviewed()` first detects a review via `pr_has_completed_review_uncached()`, it calls `fetch_review_comment_ids()` to retrieve comment IDs from the GitHub REST API.

**State**: `pr_review_comment_ids: HashMap<u64, Vec<u64>>` in `GitHubState` maps PR numbers to lists of review comment database IDs. Persisted in `daemon-state.json`.

**Effect**: On success, `Effect::MergePr` calls `gh pr merge --squash --auto` with `current_dir` set to the repo path.

**Proactive auto-merge** (`Effect::AutoMergePr`): The stuck-PR polling path in `pr.rs` emits `AutoMergePr` when `is_auto_mergeable()` returns true (approved + CI green + no conflicts + all checks complete) AND no active daemon-assigned reviewer. This proactively enables GitHub's auto-merge queue for merge-ready PRs without requiring a coworker to call `midtown pr merge`. Uses `StuckConditionType::AutoMerge` for deduplication (independent of the `MergeReady` nudge, which fires after a delay as a fallback). Both `MergePr` and `AutoMergePr` call the same `auto_merge_pr()` function.

**Pre-gate — Reviewer active**: Before the three gates, the RPC handler calls `pr_has_active_reviewer(pr_number)` on `DaemonPersistentState`. If an open `TaskSessionSpan` for a reviewer session maps to this PR, the merge is hard-blocked. This prevents the PR #1624 incident where a merge happened while the reviewer was still working. The same check gates `Effect::AutoMergePr` in the polling path.

**Workflow event gate** (`pr.approved`): The `PrApproved` workflow event is gated on the reviewer check in `action_to_effects`. When `PrContext.has_active_reviewer` is true (reviewer assigned AND review not yet cached), both the workflow event AND inline effects are suppressed — no nudge is sent and no cooldown is recorded. The `Approved` nudge cooldown (if any prior one existed) is cleared when the reviewer finishes (in `collect_reviewer_effects`) so PrApproved fires promptly on the next tick.

**Unified PR action converter** (`action_to_effects` in `pr.rs`): A single function converts `PrAction` → `Vec<Effect>`, replacing the former `pr_action_to_effects`, `comment_action_to_effects`, and `review_complete_action_to_effects` trio. For task-linked PRs, `NudgeOwner` and `SpawnOwner` actions are collapsed into `Effect::TaskPrompt` — the `deliver_task_prompt` function handles nudge-if-running / resume-if-stopped internally. For task-less PRs, `NudgeOwner` produces `NudgeSessionWithCallbacks` and `SpawnOwner` produces `SpawnCoworkerWithCallbacks` — only `PrAction::PostToChannel` maps to `Effect::PostToChannel`. Cooldown tracking (`RecordPrNudge`) and workflow event emission are preserved at call sites — `TaskPrompt` is a pure delivery mechanism.

**Effect::TaskPrompt**: Delivers a prompt to a task's session — the effect-pipeline equivalent of the `task.prompt` RPC call. Internally, `deliver_task_prompt()` checks if the session is running (nudge) or stopped (resume with fallback to fresh spawn). Accepts an optional `model` override (e.g., `"opus"` for review feedback) and an optional `TaskPromptPrContext` for observability logging. Cooldown tracking is NOT included — callers emit `RecordPrNudge` separately.

## External/Fork PR Blocking

PRs from external repositories (forks) are automatically blocked from daemon automation by default. This prevents unauthorized fork PRs from triggering reviewer spawning, nudges, task linking, or any other daemon processing.

**Detection paths**:
- **Webhook path** (primary): `handle_pull_request` extracts `head.repo.full_name` and compares against the base repo. Sets `WebhookEvent.fork_repo` with the fork's full repo name.
- **Polling path** (backstop): `detect_and_block_external_prs` in `pr.rs` compares `headRepositoryOwner.login` against `repo_owner` from `WorldSnapshot`. Uses `"{owner}/fork"` as a placeholder repo name (the polling API doesn't expose the full fork repo name).

**State** (`GitHubState`):
- `external_prs: HashMap<u64, ExternalPrInfo>` — tracks detected external PRs with source repo, title, and notification status.
- `allowed_external_prs: HashSet<u64>` — per-PR allowlist.
- `allowed_external_repos: HashSet<String>` — per-repo allowlist (matches by owner prefix for polling-detected placeholders).

**RPC methods**: `pr.allow` (allow a specific PR or all PRs from a repo), `pr.list-external` (list detected external PRs).

**CLI**: `midtown pr allow <N>`, `midtown pr allow --repo <owner/repo>`, `midtown pr list --external`.

**Cleanup**: `cleanup_closed_external_prs` is called separately from `cleanup_closed_prs` using the unfiltered open PR list (before external PRs are filtered out), ensuring blocked-but-still-open external PRs are not purged.

## Worktree Lifecycle

When a coworker is called in, the daemon creates a task-based worktree at `~/.midtown/projects/<repo>/worktrees/<branch-slug>/` via `Effect::EnsureWorktree`. The worktree is created on a named branch matching the branch slug, starting from the default branch (not HEAD). This prevents cross-PR contamination when the lead's HEAD is on an unrelated feature branch. Worktree names are decoupled from coworker identity, enabling build cache reuse across task reassignment. When worktrees for merged PRs are detected, they are cleaned up via `CleanupMergedWorktree` effects. Registry-backed stale worktrees are cleaned up via `CleanupStaleWorktree` when their age exceeds retention (using `completed_at` when present, otherwise `created_at` for abandoned entries). Disk-only orphaned worktree directories (not in the registry) are swept by `CleanupOrphanedWorktrees`, excluding `lead`, requiring retention age, and skipping paths in use by active sessions.

**Daemon state garbage collection**: On every `PrPollTick`, `check_for_state_gc()` runs independently of the stale worktree cleanup (which is gated on `retention_hours > 0`). It produces a single `GarbageCollectState` effect that atomically: (1) removes dead reviewer sessions immediately (no retention wait, since they're ephemeral and never resumed); (2) skips channel lead sessions entirely (`coworker_type="channel-lead"`) — they are long-lived and must always be available for resume by `ensure_channel_leads_alive`; (3) removes other stopped sessions (where `resume_on_startup=false`) after `worktree_cleanup_retention_hours` (default 24h) — the entire record including `initial_prompt` is dropped, since `session.clear` only needs the prompt within the retention window; (4) prunes orphaned task metadata map entries (`task_channel`, `task_model`, `task_plan`, `task_execution_skill`, `task_thread_id`, `task_message_id`) whose task IDs are not referenced by any surviving session or active task. The GC is a pure decision function in `health.rs` — no I/O, returns `Vec<Effect>` — with the effect handler in `effects.rs` performing the mutations and posting a summary to the ops channel.

**Directory layout**: All worktrees live under `~/.midtown/projects/<repo>/`:
- `worktrees/lead/` — the project lead's worktree
- `worktrees/task-<id>-<slug>/` — task-based worktrees (current)
- `sessions/` — headless session transcripts (`headless-<name>.jsonl`)
- `coworkers/<name>/` — legacy name-based worktrees (deprecated)

Old paths (`~/.midtown/worktrees/<repo>/` and `~/.midtown/coworkers/<repo>/`) are auto-migrated on first access via `migrate_worktree_paths()`. Lead session data (session ID, system prompt) formerly lived in `~/.midtown/lead/<repo>/` and is auto-migrated into `~/.midtown/projects/<repo>/` (with `lead-` prefixed filenames) via `migrate_lead_to_project()`.

**Sandbox writable paths**: Filesystem sandboxing (`sandbox-exec` on macOS, `bwrap` on Linux) restricts Claude Code writes to project-scoped directories. The `writable_dirs()` function builds the allow-list: `~/.midtown/projects/<project>/` and `~/.local/state/midtown/<project>/` (not the broad `~/.midtown/` or `~/.local/state/midtown/`). This prevents cross-project writes while leaving global config (`~/.midtown/config.toml`) readable.

## Task Completion

Tasks complete through two paths depending on whether they produce a PR:

1. **PR tasks** (most common): The coworker opens a PR and reports `midtown state idle`. The daemon auto-completes the task when the PR merges, via webhook or polling fallback. The task stays `in_progress` until merge — `task_has_open_pr()` in `rpc_coworker.rs` checks `SessionRecord.pr_number` (primary, in-memory) and the `task.pr` field on disk (secondary, survives daemon restarts). When using the disk fallback, the PR state is verified via `gh pr view` since `task.pr` is never cleared when a PR is closed — without this check, a closed/superseded PR would incorrectly block task completion.

2. **No-PR tasks** (ops, release management, investigations): The coworker reports `midtown state completed`. Since no PR exists, the daemon completes the task directly on disk (same as `task.done` RPC), clears `blocked_by` dependencies, and marks the worktree as completed. This avoids the respawn loop that occurs when a task stays `in_progress` with no PR — dispatch would repeatedly recover and respawn the coworker.

## GitHub Integration

The daemon receives real-time GitHub events via webhooks (PR creation, reviews, check runs) verified with HMAC-SHA256 signatures. PR polling runs as a backstop for missed webhook deliveries and handles time-based concerns like merge conflict detection and stuck PR identification.

### PR Coworker Attribution

PR-to-coworker resolution uses session-based lookup exclusively: PR# → task → session → `current_name`. If no session owns the PR, the owner is `None` and the daemon falls back to notifying `@user`.

Webhooks call `resolve_pr_owner_from_state()` (async, reads `DaemonState` locks). Polling calls `resolve_pr_owner()` (pure, operates on snapshot data). Both use session-only resolution.

Key functions: `resolve_pr_owner()` (pr.rs, session-only), `resolve_pr_owner_from_state()` (pr.rs, async wrapper), `coworker_from_branch()` (helpers.rs, branch → coworker map lookup, used for comment notifications and reviewer effects), `determine_pr_coworker()` (webhook.rs, frontmatter extraction for `PrOpenedInfo` only).

### PR Decision Logging

Every PR decision (both polling and webhook paths) is logged as a JSONL line to `~/.midtown/projects/<repo>/pr-decisions.jsonl`. Each entry captures the full decision context: PR number, detected issue type, owner state (active/idle), action chosen by the rules engine, and the effects emitted. The `source` field distinguishes `"polling"` from `"webhook"` triggers.

Call sites: `process_pr_issue_nudges` (polling), `collect_review_feedback_effects` (polling), `handle_pr_comment_nudge` (webhook), `handle_webhook_review_state_change` (webhook), `handle_webhook_ci_failure` (webhook). All use `log_pr_decision()` with a `PrDecisionEntry` struct.

**Coworker-authorship gate** (in `handle_pr_comment_nudge`): When review feedback arrives on a PR linked to a completed task, follow-up task creation is gated on `is_non_lead_coworker()`. Only coworker-owned PRs get auto-created follow-up tasks; lead and channel-lead PRs notify `@user` instead, avoiding spurious task churn for PRs the daemon didn't author.

This corpus enables verifying functional equivalence when migrating PR workflow logic from Rust to Python workflow scripts. Logging failures are silently swallowed — they must never crash the daemon.

## Webhook Ports

Each project daemon runs its own webhook server for GitHub integration. Port 47022 is reserved for the shared multi-project webserver. Per-project daemons auto-assign ports starting at 47023, persisting the assignment in the project's `config.toml` for stability across restarts.

## Thread Storage

Thread replies are stored in the same JSONL channel file as top-level messages, tagged with a `thread_parent_id` field referencing the parent message's ID. There is no separate index — threads are filtered in memory at read time by comparing `thread_parent_id` against the queried parent ID. Top-level messages have `thread_parent_id` omitted (serialized with `skip_serializing_if = "Option::is_none"` for backward compatibility with existing channel logs).

The `/api/channels/history` endpoint accepts an optional `thread_parent_id` query parameter:
- Absent: returns only top-level messages (where `thread_parent_id` is `None`), with `reply_count` and `last_reply` metadata when replies exist
- Present: returns only thread replies matching the given parent ID

When a thread reply is posted via `midtown channel post --thread <id>`, the daemon routes the nudge to the forked lead session that owns the thread (looked up via `topic_sessions`), but there is **no automatic broadcast to other thread participants**. If the fork wants a coworker or reviewer to see the thread reply, it **must @mention them** — otherwise they won't be notified. The fork should @mention others when a reply contains information they need to act on (e.g., a question directed at them, a decision affecting their work, or context they explicitly requested). Don't @mention for routine updates the fork can handle alone.

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
- Mermaid diagram detection and rendering (content-hash caching; web app uses mermaid-js)
- Inline ASCII art for flowchart diagrams (press number keys to open SVG in browser)
- **Type-anywhere UX**: Character keys auto-focus the input bar (like Slack/Discord)
- Tab-based focus navigation (Board → Chat → InputBar)
- Arrow keys, PageUp/PageDown, Home/End for scrolling
- Mouse support for scrolling and navigation
- Clickable hyperlinks via OSC 8 escape sequences
- Real-time token usage and cost tracking

**Data polling**:
- **Coworker state** (2s): `coworkers.status` RPC — live in-memory data, no GraphQL. Response includes `lead_working` (bool), `channel_leads_working` (map of channel name → bool), `coworkers`, `max_in_progress_tasks`, and `channel_leads`.
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

`resolve_web_dir()` (`src/lib.rs`) locates the built web-app static assets at runtime by checking three candidates in order:

1. **`exe_dir/web-app/dist`** — next to the running binary (works for release tarballs and Docker)
2. **`CARGO_MANIFEST_DIR/web-app/dist`** — source tree path baked in at compile time (works for `cargo run` and `cargo install --path .` development builds)
3. **`~/.local/share/midtown/web-app/dist`** — XDG data directory (`paths::midtown_data_dir()`), used by `install.sh` and `midtown update` for binary installs

Source-tree candidates (1, 2) are checked before the data dir (3) so that `cargo run` always serves locally built assets rather than stale binary-install assets.

The release pipeline builds the web-app once (Node 22, `npm ci && npm run build`) as a platform-independent artifact, then bundles `web-app/dist/` alongside the binary in every platform tarball. The Docker image uses a separate `node:22-slim` build stage and copies the dist into `/usr/local/bin/web-app/dist`. The curl install script (`install.sh`) and `midtown update` place `web-app/` in `~/.local/share/midtown/web-app/`, following XDG conventions for static application data.

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
- Typing indicators: The web app's `/api/status` endpoint (`web.rs`) forwards `lead_working` and `channel_leads_working` from the `coworkers.status` RPC response. `Channel.svelte` uses `lead_working` for the main channel and `channel_leads_working[channelName]` for topic channels to show typing dots when a lead or channel lead is actively working (5-second activity window based on `ProcessHealth.last_event_at`).
- Auth profile switching
  - **Per-project**: CLI sends `auth.switch` to the current project's daemon via its Unix socket. The daemon writes the project config and restarts affected sessions.
  - **Global (`--global`)**: CLI enumerates all daemon sockets in `~/.local/state/midtown/` via `enumerate_daemon_sockets()` and sends `auth.switch(all=true, force=true)` to each. The `force` flag ensures every daemon restarts its sessions even if another daemon already wrote the global config. Uses a reduced 3s per-daemon timeout to bound total CLI wait time.
  - **No-daemon fallback**: If no daemon sockets are found (or all are stale), the CLI writes the profile config directly to disk.
- Push notifications (W3C Push API with VAPID)
  - **Backend**: `src/push.rs` — VAPID key generation/storage, subscription management, encrypted push delivery via `web-push-native`. Keys stored in `~/.midtown/push/`.
  - **Frontend**: `web-app/src/lib/push.ts` — subscribe/unsubscribe flow, VAPID key fetch. `web-app/src/sw.ts` — service worker handles incoming push events with foreground suppression (skips OS notification when the app window is focused).
  - **Triggers**: Three event types fire push notifications: (1) `@user`/`@{display_name}` mentions in channel posts (`rpc_channel.rs`), (2) task completions from PR merges with `[Midtown !XX]` tags (`dispatch.rs` via `task_completed_effects`), and (3) non-Midtown PR merges (`mod.rs` webhook handler). All use `DaemonState::send_push_notification()` or `Effect::SendPushNotification`.
  - **Tags**: Each notification uses a unique tag (e.g., `pr_merged_{pr_number}`, `task_completed_{task_id}`, `mention`) to prevent the Web Notifications API from silently replacing unread notifications.
  - **Deep-linking**: Each push notification carries a `url` field (deep-link path like `/{project}?channel=web&msg=123&thread=456`) built by `dispatch::build_push_deep_link()`. On notification click, the service worker uses `postMessage({ type: "NAVIGATE", url })` to the focused client instead of `client.navigate()` for cross-platform reliability (Safari PWA). The app's `navigateToDeepLink()` function in `App.svelte` parses the URL, handles cross-project switching, and updates stores (`activeChannel`, `channelTargetMsgId`, `deepLinkMsgId`, `openThread`). When no window is open, `openWindow()` opens the deep-link URL directly and deep-linking runs via URL params on mount.
  - **HTTPS requirement**: Mobile browsers require a secure context for `PushManager`. Desktop `localhost` is exempt, but LAN access (`http://192.168.x.x`) will not have push available.
- Responsive layout with three breakpoints:
  - **Mobile (≤768px)**: Tab navigation, hamburger menu with slide-out sidebar, modal popups for task/PR details
  - **Tablet (769–1024px)**: Permanent sidebar replaces tab navigation, two-column grid layout
  - **Desktop (≥1025px)**: Three-column Slack-inspired layout with sidebar, main channel, and toggleable detail panel for tasks, PRs, and coworker info
- Clickable `@coworker` mentions in messages open coworker detail panel on desktop
- Theme-aware branding — the sidebar header logo (`App.svelte`) reactively swaps between `midtown-dark-logo.svg` and `midtown-light-logo.svg` on theme toggle. The favicon is seeded in `index.html` before first paint using the stored theme preference (`localStorage`), then kept in sync by `App.svelte`'s `$effect` block. A static `favicon.ico` fallback is kept for browsers that unconditionally request `/favicon.ico`.

### Celebration effects

Merged PRs animate across the UI once they land in `kanbanData.done`. `web-app/src/lib/CelebrationEffects.svelte` observes `$kanbanData.done` (only after `$daemonStatus` hydrates to avoid replaying historical merges), keeps a per-session set of celebrated PR keys (`repo#number`), and randomly selects one of ten short-lived CSS effects (confetti, emoji rain, matrix cascade, etc.). Effects render inside a fixed overlay with `pointer-events: none` so they never block interaction, and every particle color comes from `AVENUE_COLORS` or existing theme tokens to stay on brand.

To add a new effect:

1. Create a generator helper within `CelebrationEffects.svelte` that returns the data your effect needs (positions, characters, durations, etc.). Keep payloads serializable (no DOM references) so Svelte can diff cheaply.
2. Append a `{ type, duration, generator }` entry to `EFFECT_DEFS`. Durations should stay under ~5 s and should match the CSS animation timing you introduce.
3. Add a new markup branch inside the `{#each activeEffects}` block plus scoped styles/keyframes for the effect. Reuse the overlay semantics (absolute positioning, opacity fades, `pointer-events: none`) so rapid merges cannot degrade layout performance.
4. Whenever possible, pull palette values from `COLOR_PALETTE`/`AVENUE_COLORS` instead of hardcoding new hex codes. This keeps dark/light themes and future recolors consistent.

## Tool Call Display

Tool call data is displayed via `msg.tool_data` on channel messages. The `extract_tool_blocks()` function in `stream.rs` pairs `tool_use` blocks from Assistant events with `tool_result` blocks from User events by `call_id`, producing `ToolBlock` structs that are attached to `PostToChannel` effects. Both topic channel messages (via `process_lead_output()`) and DM channel messages (via `process_agent_output()`) carry `tool_data`, giving the web UI and TUI a single, persisted source for rendering tool call activity.

**Data flow:**
```
StreamEvent (NDJSON drain) → extract_tool_events() → Vec<ToolBlock>
    → Effect::PostToChannel { tool_data } → channel message with ToolBlocks
    → DaemonState.tool_activity_headers (pre-formatted strings per agent)
    → coworkers.status RPC → TUI reads header strings directly
```

- **Channel messages**: Tool activity is carried as `tool_data` (structured `ToolBlock` JSON) on `PostToChannel` effects. The web UI reads `msg.tool_data` directly from channel messages. When a channel message with `tool_data` is posted, the daemon also updates `DaemonState.tool_activity_headers` — a `RwLock<HashMap<String, Vec<String>>>` of pre-formatted display strings (e.g. `"✓ read foo.rs"`, `"› $ git status"`).
- **TUI rendering**: The TUI polls `coworkers.status` (at 2s intervals) which calls `collect_tool_activity()` to read `tool_activity_headers`. The TUI renders a compact activity strip at the bottom of the chat pane showing the most recent tool calls per active agent, using these pre-formatted header strings directly.
- **Lifecycle**: Tool activity headers for an agent are cleared from `tool_activity_headers` when the agent posts a non-tool channel message (work phase done) and when a coworker session stops (via `cleanup_coworker_state`).

## DM Channel Streaming

Agents without a native home channel get a DM mirror (`dm-<name>`). In practice this means coworkers and reviewers. Root leads and fork sessions do not get DM mirrors — leads already stream to their main/topic channel, and forks stream to their bound thread, so DM copies would be duplicate noise.

**Data flow:**
```
StreamEvent (NDJSON drain) → extract_assistant_text() → aggregated text
                           → extract_tool_blocks()    → Vec<ToolBlock> (structured)
                           → detect_provider()        → Option<String> ("claude" | "codex")
    → process_lead_output()     → Effect::PostToChannel { channel: Some("<name>"), tool_data, provider }
                                  (main lead → main channel, channel leads → topic channels, forks → bound topic channels)
    → process_agent_output()    → Effect::PostToChannel { channel: Some("dm-<name>"), tool_data, provider }
                                  (coworkers and reviewers only)
    → channel JSONL file + WebSocket broadcast
```

- **`auto_output` flag**: `Message`, `Effect::PostToChannel`, and `ChannelMessageData` carry an `auto_output: bool` field. Only `stream.rs` (`process_lead_output()` and `process_agent_output()`) sets it to `true` — all other code paths (explicit `midtown channel post`, system messages, nudges) default to `false`. The web UI uses this to apply muted styling (dimmed text + left border) to streamed output, creating visual hierarchy between intentional posts and background output.

- **`process_agent_output()`** (`daemon/stream.rs`): Takes the set of active agent session names selected for DM mirroring and posts each agent's aggregated text output to `dm-<name>`. The caller excludes the project lead, root channel leads, and fork sessions, so DM mirroring covers coworkers and reviewers. For messages containing tool calls, the effect carries empty `content` with structured `tool_data: Vec<ToolBlock>` and `provider: String` — each client renders tool data its own way (the TUI generates a `[Bash, Read]` text summary locally, the web app renders rich expandable blocks).
- **Structured tool data** (`ToolBlock` in `message.rs`): Preserves raw tool call JSON (`tool_name`, `input`, `output`, `error`) extracted from stream events. `extract_tool_blocks()` pairs `tool_use` blocks from Assistant events with `tool_result` blocks from User events by `call_id`. `detect_provider()` identifies the AI provider (`"claude"` or `"codex"`) from stream event metadata. Both fields are `Option` with `serde(default)` for backward compatibility with legacy messages.
- **Nudge content**: When a coworker receives a nudge (task assignment, mention, review, etc.), the nudge message is also posted to `dm-<name>` via `Effect::PostToChannel`. This makes nudge conversations visible in the DM channel alongside coworker output. `DmFromUser` nudges are excluded because the user's message is already written to the DM channel by the RPC post handler before the nudge effect fires. Fork sessions are also excluded — only pool coworker names receive DM posts.
- **Task separator**: A `PostSystemMessage` separator is posted to `dm-<name>` to visually delineate task boundaries. For regular coworkers this uses the format "─── Task !42: Fix auth bug ───" and happens in three paths: (1) `SpawnSession` when a session spawns, (2) `AssignAndSpawn` when dispatching a new task, and (3) `task.claim` RPC when a coworker self-claims a pending task. Session recovery via `SpawnSession` with `resume=true` omits the separator since one was already posted on initial assignment. Reviewer sessions receive a PR-based separator (e.g., "─── Reviewing PR #42 ───") in their `SpawnCoworkerWithCallbacks` `on_success` effects.

## Workflow Script System

> **User-facing guide:** See [Writing Custom Workflow Scripts](workflow-customization.md) for a tutorial with examples, the full event reference, RPC methods, and testing instructions. This section documents the internal architecture.

Each channel can have a `workflow.py` script that controls how the daemon responds to domain events — PR lifecycle, coworker status changes, task transitions, CI results, and more. Scripts are invoked by the daemon using the [Midtown Python SDK](../sdk/python/), either as a persistent sidecar process or via `uv run` subprocess per event (automatic fallback).

**Authoritative for PR lifecycle**: For the 5 PR lifecycle events (`pr.approved`, `pr.changes_requested`, `pr.ci_failed`, `pr.ci_passed`, `pr.conflict`), the workflow script is the **sole authority** when a channel + task association exists. The daemon emits cooldown tracking (`RecordPrNudge`) and the workflow event — the script handles nudging via `rpc.nudge_coworker()`. This means overriding `pr.approved` in a channel's `workflow.py` fully controls what happens when a PR is approved. For PRs without channel/task associations, the daemon's compiled-in inline effects are preserved as a fallback.

### Script Resolution

`workflow_script_for_channel()` in `src/paths.rs` resolves the active script using a 4-level priority order (first file found wins):

1. `<project_root>/.midtown/channels/<channel>/workflow.py` — channel-specific, committed to repo
2. `~/.midtown/projects/<repo>/channels/<channel>/workflow.py` — channel-specific, local only
3. `<project_root>/.midtown/workflow.py` — project default, committed to repo
4. `~/.midtown/projects/<repo>/workflow.py` — project default, local only

If no script is found, or if a PR has no channel/task association, the daemon falls back to its compiled-in inline effects. This layered resolution allows teams to commit shared workflows to the repo while maintaining machine-specific local overrides.

### Invocation

The daemon emits `Effect::EmitWorkflowEvent` at detection points in `pr.rs`, `health.rs`, `dispatch.rs`, `rpc_task.rs` (task creation → `TaskCreated`), and `rpc_channel.rs` (channel posts → `CoworkerMessage` / `ChannelMessage`). `invoke_workflow_script()` in `effects.rs` tries the sidecar fast path first (see below), falling back to subprocess invocation:

```
uv run workflow.py --event '{"type":"pr.opened",...}' \
    --state ~/.midtown/projects/<repo>/channels/<channel>/workflow-state.json \
    --socket ~/.local/state/midtown/<repo>/daemon.sock
```

**Changes take effect on the next daemon tick** — no daemon restart required. If a script file is modified while a sidecar is running, the sidecar detects the mtime change and restarts automatically (hot-reload).

### Persistent Sidecar Mode

`WorkflowSidecarManager` (`src/daemon/sidecar.rs`) maintains long-lived Python sidecar processes, one per workflow script. Instead of spawning a new `uv run` subprocess per event (~300-800ms overhead from Python startup + imports), events are sent as newline-delimited JSON on stdin and the sidecar responds with `{"ok":true}` on stdout (~5-20ms per event).

**Lifecycle:**
- **Lazy spawn**: On the first event for a script, the manager spawns `uv run workflow.py --sidecar` and waits for `{"ready":true}` on stdout (15s timeout).
- **Automatic fallback**: If the script doesn't emit the ready signal (e.g., it uses `run()` instead of `run_loop()`), it's marked `single_shot_only` and all future events use subprocess-per-event.
- **Hot-reload**: When the script file's mtime changes, the sidecar is killed and re-spawned. This also clears the `single_shot_only` flag, so upgrading a script from `run()` to `run_loop()` takes effect without a daemon restart.
- **Crash restart**: Exponential backoff (500ms base, doubling per crash, capped at 60s). During backoff, events fall back to subprocess.
- **Shutdown**: On daemon shutdown, stdin is closed (signaling EOF) with a 3s grace period before kill.

**Concurrency:** Per-entry `tokio::sync::Mutex` locks allow events to different scripts to proceed concurrently. The outer `std::sync::Mutex` on the HashMap is held only briefly for lookups/inserts.

**State field:** `DaemonState.workflow_sidecar: WorkflowSidecarManager` — initialized with the daemon socket path, health-checked on the session drain interval, shut down during daemon cleanup.

### State Persistence

Workflow state is owned by the daemon in-memory (`DaemonPersistentState::workflow_state`) and persisted to `daemon-state.json` alongside other daemon state. This is a `HashMap<String, serde_json::Value>` keyed by channel name. Each channel's state is further namespaced by plugin key when plugins use the `plugin` parameter.

Two RPC methods in `rpc_workflow.rs` provide access:

- **`workflow.get_state`** — reads from the in-memory `workflow_state` map for a channel, optionally scoped to a plugin key. Returns `null` when absent.
- **`workflow.set_state`** — updates the in-memory state and persists to `daemon-state.json`. With a `plugin` key, merges at that key; without, replaces the entire channel state. Concurrent writes are serialized by the `persistent_state` Mutex.

**Legacy migration:** On first startup after upgrade, per-channel `workflow-state.json` files are automatically migrated into `daemon-state.json` and the old files are removed. The `--state` CLI flag is still passed to legacy `workflow.py` scripts for backward compatibility, but new plugin-based workflows should use RPC exclusively.

**Lead-driven mode:** Channels can be put into "lead-driven" mode via `workflow.set_lead_driven` RPC (CLI: `midtown workflow lead-driven <channel>`). When enabled, the daemon relays workflow events as human-readable @mentions to the channel lead instead of executing its built-in state machine (auto-dispatch, reviewer spawning, PR nudges). State is stored in `DaemonPersistentState::lead_driven_channels: HashSet<String>` and exposed to pure decision functions via `WorldSnapshot::lead_driven_channels`. The web API extends `GET /api/channels/{channel}/workflow` with a `lead_driven` boolean and `PUT /api/channels/{channel}/workflow` accepts `{"lead_driven": true/false}`.

Effect gating for lead-driven channels spans three decision modules:

- **Dispatch** (`dispatch.rs`): `dispatch_via_sessions_inner`, `dispatch_owned_pending_tasks`, and `dispatch_unowned_pending_tasks` skip coworker spawning/nudging for tasks in lead-driven channels. Merged-PR auto-complete runs *before* the lead-driven check so task lifecycle cleanup still works.
- **PR actions** (`pr.rs`): `action_to_effects` replaces inline effects (NudgeOwner, SpawnOwner) with `RecordPrNudge` + `EmitWorkflowEvent` when the PR's channel is lead-driven. For task-linked PRs, `NudgeOwner` and `SpawnOwner` are collapsed into `Effect::TaskPrompt` — `deliver_task_prompt` handles nudge-if-running / resume-if-stopped internally. Reviewer spawning is gated in `collect_reviewer_effects_with_source`. Stuck-condition scenarios (`collect_stuck_condition_effects`) skip PRs in lead-driven channels entirely.
- **Effect execution** (`effects.rs`): `EmitWorkflowEvent` dispatches to either the workflow plugin daemon (when a script exists) or posts a human-readable @mention to the channel lead (lead-driven mode). The `PrContext::lead_driven_channels` field provides the lookup for all PR-related gating via `is_lead_driven(pr_number)`.

### Plugin Daemon (Unix Socket IPC)

`WorkflowDaemon` (`sdk/python/midtown/daemon.py`) is a long-lived Python process that serves plugin hook dispatch over a Unix domain socket. The Rust daemon connects to it to dispatch events to pluggy-based plugins.

**Protocol:** Newline-delimited JSON, one request per connection. Each connection sends one request and receives one response, then closes.

Request format (event dispatch):
```json
{"type": "pr.opened", "event": {...}, "task_id": "7", "task_state": "in_review"}
```

Response format (event dispatch):
```json
{"ok": true, "actions": [...], "default_prevented": false}
```

Request format (reload command):
```json
{"type": "reload"}
```

Response format (reload):
```json
{"ok": true, "reloaded": true, "loaded_plugins": ["/path/to/plugin.py", ...]}
```

The Rust side uses `PluginDispatchResult` for event responses and `PluginReloadResult` for reload responses, keeping the deserialization types aligned with their semantics.

**Startup handshake:** On startup, the daemon writes `{"ready":true}\n` to stdout so the Rust parent process knows the socket is accepting connections.

**Hot-reload (two-tier approach):**
- **Per-event mtime check:** Before processing each event dispatch, `_process_request()` calls `reload_changed()` to detect mtime changes in already-tracked plugin files and re-register modified modules. This does NOT scan for new plugins to avoid unnecessary `iterdir()` overhead on every event.
- **Periodic full scan:** The Rust event loop runs a `plugin_scan_interval` timer every 5 seconds that: (1) calls `update_plugin_dirs()` to detect new/removed plugin directories (restarting the daemon if dirs changed), and (2) sends a `"reload"` IPC command which triggers both `reload_changed()` AND `scan_for_new_plugins()` to discover newly added plugin files within existing directories.

**Stale socket cleanup:** On startup, any existing socket file at the configured path is unlinked before binding, preventing "address already in use" errors from prior crashes.

**CLI entry points:** `python -m midtown` (via `__main__.py`) or `python -m midtown.daemon` (via `if __name__ == "__main__"` guard in `daemon.py`). Both accept `--socket-path` and `--plugin-dirs` arguments.

### Plugin Daemon Lifecycle Manager (Rust)

`PluginDaemonManager` (`src/daemon/plugin_daemon.rs`) manages the lifecycle of the Python plugin daemon process from the Rust side. It follows a similar pattern to `WorkflowSidecarManager` but manages a single process (not per-script).

**Spawning:** Runs `uv run --project <sdk_path> python -m midtown --socket-path <path> --plugin-dirs <dirs>`. The SDK path is resolved via `paths::resolve_python_sdk_dir()` which checks: next to executable → `CARGO_MANIFEST_DIR/sdk/python` → `~/.local/share/midtown/sdk/python`.

**Ready handshake:** After spawning, waits up to 30s for `{"ready":true}` on stdout, confirming the Unix socket is accepting connections.

**Health monitoring:** On each session drain interval, `check_health()` calls `try_wait()` on the child process to detect exits. If the process has exited, it records a crash and enters backoff.

**Restart with backoff:** Exponential backoff starting at 500ms, doubling per consecutive crash, capped at 60s. Backoff resets on successful ready handshake. `ensure_running()` on each drain tick attempts restart when the backoff period has elapsed.

**Plugin discovery:** `paths::discover_plugin_dirs()` scans for plugins in up to four directories (priority order): channel-specific in-repo, channel-specific local, project-wide in-repo, project-wide local. A directory is considered to contain plugins if it has at least one `.py` file (not `_`-prefixed) or a subdirectory with a `SKILL.md` file (AgentSkills format). At startup, only project-wide paths are scanned. At runtime, when dispatching workflow events, channel-specific plugin directories are discovered and merged into the running daemon via `merge_plugin_dirs()`. The manager only starts when at least one directory has plugin files.

**AgentSkills format:** Plugin directories can contain AgentSkills — subdirectories with a `SKILL.md` frontmatter file specifying `midtown_hooks` (path to hooks module, default `scripts/hooks.py`) and `midtown_order` (execution priority). The `skill.py` module (`sdk/python/midtown/skill.py`) parses this frontmatter using a minimal YAML subset parser (no PyYAML dependency). `WorkflowDaemon` registers each AgentSkills plugin with a unique name (`agentskills_{name}`) to prevent pluggy conflicts when multiple skills share the same hooks filename.

**Periodic plugin scan:** The main event loop runs a `plugin_scan_interval` (5s) that calls `update_plugin_dirs()` to re-discover plugin directories. If directories changed, the Python daemon is killed and `ensure_running()` spawns a fresh one (which loads all plugins on startup, so no reload is needed). If directories are unchanged, `send_reload()` sends a `"reload"` IPC command so the Python side checks for file-level changes and new plugins. `update_plugin_dirs()` returns a boolean indicating whether dirs changed, preventing a redundant (and potentially racy) reload immediately after a daemon restart.

**Shutdown:** On daemon exit, sends SIGTERM to the child process and waits up to 3s for graceful exit, then escalates to SIGKILL if needed, then cleans up the socket file.

**State field:** `DaemonState.plugin_daemon: PluginDaemonManager` — initialized during `DaemonState::new()` with discovered plugin dirs, health-checked on the session drain interval alongside the sidecar manager, shut down during daemon cleanup.

### Python SDK

The Midtown Python SDK (`sdk/python/midtown/`) provides `run()` (single-shot) and `run_loop()` (persistent sidecar) entry points, plus the `MidtownRPC` client. `run()` auto-detects `--sidecar` in `sys.argv` and delegates to `run_loop()`, so existing scripts gain sidecar support without code changes. A typical workflow script:

```python
from midtown import run, MidtownRPC

def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    if event["type"] == "coworker.idle":
        rpc.post_to_channel(f"{event['coworker']} is idle")

if __name__ == "__main__":
    run(handle)
```

`MidtownRPC` methods: `post_to_channel()`, `spawn_coworker()`, `spawn_reviewer()`, `nudge_coworker()`, `create_task()`, `update_task()`, `complete_task()`, `list_tasks()`, `check_pending()`, `get_state()`, `set_state()`.

### Hook-Based Plugin API

The SDK provides a [pluggy](https://pluggy.readthedocs.io/)-based hook system (`sdk/python/midtown/hooks.py`, `sdk/python/midtown/actions.py`) for extending daemon behavior without modifying workflow scripts directly.

**Core types:**

- `WorkflowHooks` — 18 hook specs covering task lifecycle (`on_task_created`, `on_task_assigned`, `on_task_completed`, `on_task_phase_complete`), PR lifecycle (`on_pr_opened`, `on_pr_approved`, `on_pr_changes_requested`, `on_pr_merged`, `on_pr_ci_passed`, `on_pr_ci_failed`, `on_pr_conflict`, `on_pr_auto_merge`), coworker events (`on_coworker_spawned`, `on_coworker_idle`, `on_coworker_stuck`, `on_coworker_message`), fork leads (`on_fork_lead_spawned`, `on_fork_lead_idle`), channel messages (`on_channel_message`), and timer ticks (`on_timer_tick`).
- `TaskHooks` — 3 hook specs for customizing task prompts: `get_system_prompt()`, `get_author_prompt()`, `get_reviewer_prompt()`.
- `HookContext` — Context object passed to every workflow hook invocation. Fields: `event_type`, `event`, `task_id`, `task_state`, `prev_task_state`, `coworker`, `pr_number`, `channel`, `state`, `actions`. Plugins can call `ctx.prevent_default()` to suppress the daemon's built-in behavior; check with `ctx.is_default_prevented()`.
- `DaemonAction` — A side-effect command returned by hook implementations (frozen dataclass with `method` and `params`).
- `DispatchResult` — Return type of `WorkflowDaemon.dispatch_event()`. Contains `actions: list[DaemonAction]` and `default_prevented: bool`.
- `Actions` — Factory class for constructing `DaemonAction` objects. Methods: `post_to_channel()`, `nudge_coworker()`, `spawn_reviewer()`, `spawn_coworker()`, `complete_task()`, `enable_auto_merge()`, `check_pending()`, `create_task()`, `update_task()`.

**Plugin registration:** Plugins are classes with `@hookimpl`-decorated methods matching the hook spec signatures. Multiple plugins can respond to the same hook — pluggy collects all return values.

```python
from midtown import hookimpl, HookContext, DaemonAction

class MyPlugin:
    @hookimpl
    def on_pr_opened(self, ctx: HookContext) -> list[DaemonAction]:
        return [ctx.actions.post_to_channel(f"PR #{ctx.pr_number} opened!")]
```

**Exports:** All hook types are re-exported from the top-level `midtown` package: `Actions`, `DaemonAction`, `DispatchResult`, `HookContext`, `WorkflowHooks`, `TaskHooks`, `hookimpl`, `hookspec`.

### Reference Implementation

`sdk/python/midtown/default_workflow.py` is the reference implementation that **drives** the PR lifecycle for task-linked PRs. The daemon delegates PR nudging to this script when a channel + task association exists. Copy it to start customizing:

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

**Task event enrichment:** Task lifecycle events (`task.created`, `task.assigned`, `task.completed`) include `subject`, `description`, `thread_id`, and `message_id` fields. `thread_id` and `message_id` let workflow scripts post responses into the correct Slack/IRC thread. For `task.created`, `thread_id` defaults to the announcement message ID when no explicit `--thread-id` is given, so scripts always have a thread to reply in. Message events (`coworker.message`, `channel.message`) include `thread_id` and `message_id`. The pure decision functions in `dispatch.rs` obtain thread/message context via `WorldSnapshot.task_thread_id_map` and `WorldSnapshot.task_message_id_map` (populated from `DaemonPersistentState`). The webhook path in `mod.rs` passes context through `dispatch::TaskEventContext`.

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
- `attached_coworkers: Mutex<HashMap<String, DateTime<Utc>>>` — Tracks interactive attach/detach state for headless coworkers. Keys are coworker names; values are the attach timestamp. Entries are added on `midtown agent attach`, removed on `midtown agent detach` or via `Effect::AutoDetachCoworker` (auto-detach after `ATTACH_TIMEOUT` = 10 min, to recover from crash/disconnect without detach).

## Coworker Questions (AskUserQuestion)

Coworkers can ask the Lead questions via the Claude Code `AskUserQuestion` tool. The flow:

1. Coworker calls `AskUserQuestion` → Claude Code CLI runs `midtown agent asking` → daemon RPC `coworker.asking`
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
- **Lazy channel creation** (`mod.rs`): `send_and_broadcast_async()` broadcasts `channel_list_changed("created")` when `ChannelRouter::send()` returns `is_new = true`, indicating the channel was lazily opened for the first time. This covers DM channels (`dm-<name>`) and any other channels auto-created by the first `PostToChannel`/`PostSystemMessage` write.

**Web app handling** (`api.ts`): The `channel_list_changed` WebSocket event triggers `fetchChannels()` to re-fetch the full channel list from the REST API (source of truth), rather than optimistically updating client-side state.

**TUI**: The TUI creates channels via daemon RPC when available (falling back to direct filesystem access if the daemon is unreachable), so the daemon handles broadcasting. The TUI's 30-second channel poll handles updates from other clients.

## Reviewer Channel Routing

Reviewers inherit the topic channel of the task associated with their PR. The data flow is: PR number → `pr_task_index.task_for_pr()` → task ID → `task_channel` → channel name → `LaunchConfig.channel` → `MIDTOWN_CHANNEL` env var. This ensures `midtown channel post` from within a reviewer session routes to the task's topic channel instead of the main channel.

**Escalation target routing**: When a reviewer's channel has an active channel lead, `LaunchConfig.escalation_target` is set to the channel name. The escalation target is communicated to the reviewer through two redundant mechanisms (belt-and-suspenders): (1) the `{escalation_target}` template variable in the reviewer system prompt resolves to the channel lead, and (2) the initial prompt is regenerated via `reviewer_launch_prompt()` with the escalation target, adding an explicit "Address review notes to @{target}" line. This ensures the reviewer knows who to address even if system prompt substitution fails. Falls back to the project name when no channel lead exists, with a `warn!` log for diagnosis.

The lookup happens at five spawn/respawn sites:
- **Initial spawn** (`pr.rs`, `collect_reviewer_effects_with_source`): Uses `PrContext::routing_only()` to read channel routing data in a single lock acquisition before the per-PR loop, then calls `ctx.get_channel(pr_number)`. Resolves `escalation_target` from `channel_lead_names`.
- **Stuck restart** (`health.rs`, `build_reviewer_respawn_effects`): Uses `WorldSnapshot::channel_for_pr()` — the synchronous equivalent for decision functions operating on the snapshot. Resolves `escalation_target` from `WorldSnapshot::channel_lead_names()`.
- **Dead restart** (`health.rs`, `build_reviewer_respawn_effects`): Same path as stuck restart.
- **Daemon recovery** (`startup.rs`, `recover_from_session_records`): Restores `channel` from `SessionRecord.channel` and resolves `escalation_target` from `channel_lead_names()`.
- **Auth rotation** (`rpc_auth.rs`): Captures reviewer channels from session records before relaunch and resolves `escalation_target` from `channel_lead_names()`.

`WorldSnapshot::channel_for_pr()` and `PrContext::get_channel()` are parallel implementations of the same two-step lookup for the sync (snapshot) and async (persistent state) contexts respectively.

## Task ID Injection (MIDTOWN_TASK_ID)

Spawned sessions receive a `MIDTOWN_TASK_ID` env var containing the numeric task ID (e.g., `"2113"`). The data flow is: `LaunchConfig.task_id` → env var injection in `to_cli_args()`/`to_shell_command()` → `MIDTOWN_TASK_ID` in the spawned process.

**Auto-threading**: When `MIDTOWN_TASK_ID` is set and the coworker runs `midtown channel post` without `--task` or `--thread`, the CLI automatically threads the message under the task's announcement. If the task no longer exists (stale ID), it gracefully falls back to a regular channel post.

**Spawn sites that set `task_id`**:
- **Coworker dispatch** (`dispatch.rs`): From the assigned task ID
- **PR auto-pilot** (`pr.rs`, `action_to_effects`): Via `Effect::TaskPrompt` for task-linked PRs, from `PrContext.pr_task_associations`
- **Reviewer spawn** (`pr.rs`, `collect_reviewer_effects_with_source`): From `PrContext.pr_task_associations`
- **Task handoff** (`rpc_task.rs`, `handle_task_handoff`): Swaps the agent type on a task's session via `--resume --agent`
- **Reviewer follow-up resume** (`pr.rs`, `handle_pr_comment_activity`): From `pr_to_task_map()`
- **Daemon recovery** (`dispatch.rs`): From the recovery task ID

## TaskSessionSpan — Temporal Session Tracking

`TaskSessionSpan` is the single source of truth for which session is actively working on a task. It replaces the former `pr_reviewers` / `task_reviewer_metadata` dual model.

**Structure** (defined in `src/daemon/state.rs`):

```
TaskSessionSpan {
    task_id:    String,              // task this span belongs to
    agent_name: String,              // coworker name at spawn time
    agent_type: String,              // "dev", "reviewer", or "channel-lead"
    session_id: String,              // Claude Code session ID
    start_time: DateTime<Utc>,
    end_time:   Option<DateTime<Utc>>,  // None = still active
}
```

**Storage**: `task_session_spans: Vec<TaskSessionSpan>` on `DaemonPersistentState`. Open spans (`end_time = None`) represent active assignments; closed spans are retained for history and pruned when the list exceeds 500 entries or spans are older than `worktree_cleanup_retention_hours`.

**Query helpers** on `DaemonPersistentState`:
- `active_span_for_task(task_id)` — returns the open span for a task, if any
- `active_reviewer_for_pr(pr_number)` — finds an open reviewer span whose task maps to this PR via `task_pr_number`
- `pr_has_active_reviewer(pr_number)` — boolean wrapper around `active_reviewer_for_pr`
- `active_reviewer_spans()` — all open reviewer spans (used for health monitoring)

**Reviewer-specific per-task metadata** lives in separate maps on `DaemonPersistentState` (keyed by task ID, not PR number):
- `task_placeholder_comment_id: HashMap<String, u64>` — GitHub comment ID of the "Review in progress" placeholder
- `task_restart_count: HashMap<String, u32>` — how many times the reviewer was restarted for this task
- `task_pr_number: HashMap<String, u64>` — maps task ID to PR number (set at task creation, used by `active_reviewer_for_pr`)

**Effects**:
- `Effect::CreateTaskSessionSpan { task_id, agent_name, agent_type, session_id }` — opens a new span
- `Effect::CloseTaskSessionSpan { task_id, session_id }` — sets `end_time` on the matching open span

## Reviewer Health and Stuck Detection

Reviewers are headless Claude Code sessions assigned to specific PRs. The daemon monitors them for stuck conditions (alive but unresponsive) and dead conditions (process exited before posting a review).

### Dead Reviewer Detection

`check_and_restart_dead_reviewers()` detects reviewers whose process has exited (is_alive = false) without posting a review. This catches natural exits (max turns, context window full) before the review is complete. Dead reviewers are respawned up to `MAX_REVIEWER_RESTARTS` times.

### Placeholder Comment Handling

The daemon owns the full lifecycle of placeholder comments — both posting and updating. This avoids prompt-compliance issues (e.g., some Claude models escape `!` characters, producing `<\!--` which breaks tag matching).

**Posting**: When a reviewer spawns, `collect_reviewer_effects_with_source()` chains an `Effect::PostPrComment` in the `on_success` callback of `SpawnCoworkerWithCallbacks`. The effect first checks for an existing placeholder via `lookup_existing_placeholder()` (which reuses the same 3-tier resolution described below). If found, it edits the existing comment via `gh api --method PATCH`; otherwise, it creates a new comment via `gh pr comment` and parses the comment ID from the stdout URL. Either way, the comment ID is stored in `task_placeholder_comment_id` (keyed by task ID) on `DaemonPersistentState` and cached in `reviewer_placeholder_cache`.

**Updating with review findings**: The reviewer agent calls `midtown pr review post --pr <N> --body-file <path>` when its review is complete. The `pr.review-post` RPC handler wraps the body with `<!-- midtown: <name> -->` frontmatter and the Midtown footer, then patches the comment via `gh api --method PATCH`. Errors are surfaced to the caller so the reviewer agent can retry.

**Three-tier placeholder ID resolution**: When the daemon needs a placeholder comment ID (for snapshot collection or review posting), it checks in order:
1. `task_placeholder_comment_id[task_id]` on `DaemonPersistentState` (stored when the daemon posted it)
2. `reviewer_placeholder_cache` (TTL: 120s, populated by prior lookups)
3. `pr_in_progress_placeholder_comment_id()` API call (fallback for comments posted before this feature)

Placeholder comments include a `<!-- midtown-placeholder -->` HTML tag. This tag is checked by `is_placeholder_comment()` in `webhook.rs` — both `handle_issue_comment` and `handle_review_comment` return `None` when the tag is present, suppressing `pr_activity` generation and preventing false nudges to the PR owner while the review is still in progress. The tag naturally disappears when the reviewer posts final results (replaced by `<!-- midtown: name -->`).

When a reviewer is restarted (stuck or dead) and had previously posted a placeholder, the daemon patches the comment via `Effect::UpdatePrComment` to indicate the reviewer timed out and a replacement was assigned. This keeps the PR timeline informative.

**WorldSnapshot fields** (reviewer fields are in the `reviewer: SnapshotReviewerState` sub-struct):
- `reviewer.reviewer_in_progress_comment_ids: HashMap<u64, u64>` — Maps PR number to the GitHub comment ID of a dangling "Review in progress" placeholder comment. Collected during `collect_world_snapshot()` using `reviewer_placeholder_cache` (TTL: 120s for all entries, both positive and negative). Used by health functions to emit `UpdatePrComment` effects marking abandoned placeholders.
- `reviewer.reviewer_restart_counts: HashMap<u64, u32>` — Maps PR number to the number of times a reviewer has been restarted for that PR. Derived from `task_restart_count` (keyed by task ID) via `task_pr_number` during snapshot collection.
- `reviewer.reviewer_escalations_posted: HashSet<u64>` — Tracks PRs for which a max-restart escalation warning has already been posted (prevents repeated spam).
- `recently_recovered_session_ids: HashSet<String>` — (top-level) Session IDs for which a recovery was recently attempted and succeeded. Pre-evaluated from `state.cooldowns` (category `"session_recovered"`, keyed by session ID) so decision functions stay pure. Used by both `dispatch_via_sessions` (Path 1) and `spawn_for_pending_tasks` (Path 2) to skip re-recovery when a session dies quickly after being recovered, preventing log spam on every 5s tick.
- `is_at_task_limit: bool` — (top-level) Pre-computed from `in_progress_tasks.len() >= max_in_progress_tasks` during `collect_world_snapshot()`. Used by pure decision functions in `dispatch.rs` and `pr.rs` to gate new spawns without performing I/O. All task types (dev, reviewer, ops) share this single limit — there is no separate `REVIEW_HEADROOM` or dev-vs-reviewer distinction. Orphaned tasks (in-progress but no running coworker) count toward the limit while recovery is in progress. For RPC handlers outside the snapshot pipeline, `DaemonState::is_at_task_limit()` reads from disk directly.
- `pr_protected_tasks: HashSet<String>` — (top-level) Task IDs that should not be spawned or recovered due to PR status or task completion. Pre-computed from `all_tasks` during `collect_world_snapshot()` via `is_task_pr_protected()`. Checks (in order): (1) task completed → always protected; (2) merged PR (via `pr_task_index.session_pr_for_task()` or `task.pr` in merged cache) → always protected regardless of session state (prevents recovery-loops); (3) owner not in `active_names` → not protected by open PRs (allows dispatch of pending tasks or tasks whose owner went away); (4) task has an open PR (via `pr_task_index.session_pr_for_task()`); (5) GitHub PR title pattern match (via `pr_task_index.github_pr_for_task()`). Dispatch functions use `snap.pr_protected_tasks.contains()` instead of per-call-site guard functions.

**DaemonState fields:**
- `reviewer_placeholder_cache: Mutex<HashMap<u64, (Option<u64>, Instant)>>` — Cache for `pr_in_progress_placeholder_comment_id()` lookups. Maps PR number to `(comment_id_or_none, checked_at)`. Both positive and negative entries expire after `PLACEHOLDER_CACHE_TTL_SECS` (120s). Cleared when `mark_reviewed_pr()` is called to ensure freshness after a review is posted.

**Effects:**
- `Effect::PostPrComment { pr_number, reviewer_name, body }` — Posts or reuses a placeholder comment on a PR. Uses `lookup_existing_placeholder()` (3-tier resolution) to check for an existing placeholder; if found, edits it via `gh api --method PATCH`, otherwise creates a new one via `gh pr comment`. Stores the comment ID in `task_placeholder_comment_id` on `DaemonPersistentState`. Chained as an `on_success` callback when spawning a reviewer.
- `Effect::UpdatePrComment { comment_id, repo_full_name, new_body }` — Patches an existing GitHub issue comment via `gh api --method PATCH`. Used to update stale "Review in progress" placeholder comments when a reviewer is restarted due to being stuck or dead, and by `pr.review-post` to replace the placeholder with final review findings.

## Reminders

The Lead can set reminders that trigger on specific conditions or cron schedules:

```bash
# Remind me when all tasks are done and PRs merged
midtown channel remind all-work-merged "Time to deploy!"

# Cron-based reminder (UTC) — fires on schedule
midtown channel remind cron "0 9 * * MON" "Monday standup"

# Repeat policies: --repeat 0 (once, default for all-work-merged),
#   --repeat -1 (indefinite, default for cron), --repeat N (N additional fires)
midtown channel remind cron "*/30 * * * *" "Check deploy" --repeat 3

# List active reminders
midtown channel remind list

# Cancel a reminder
midtown channel remind cancel <id>
```

**Trigger types:**
- `AllWorkMerged` — fires when no pending/in-progress tasks and no open coworker PRs.
- `CronUtc` — fires on a cron schedule (evaluated in UTC). Uses window-based matching: the daemon checks if the next cron occurrence after `last_evaluated_at` falls within the current tick window, preventing double-fires and missed fires across ~30s tick intervals.

**Repeat policy:** Reminders can fire once (`RepeatPolicy::Once`), a fixed number of additional times (`Times(N)` = N+1 total fires), or indefinitely (`Indefinite`). The `fire_count` field tracks how many times a reminder has fired.

Reminders are stored in `daemon-state.json` and evaluated by the daemon each tick.


## Self-Update (`midtown update`)

`src/bin/midtown/cli/update.rs` implements self-update via GitHub releases. Two entry points:

- **`handle_update(check_only)`** — Interactive update triggered by `midtown update`. Downloads the platform-specific tarball to a `tempfile::TempDir` (RAII cleanup on all error paths), extracts it, then replaces the binary and web-app directory. The web-app swap uses atomic `fs::rename()` (matching `install.sh`) rather than recursive copy, so a crash mid-update never leaves a partial `web-app/` directory.
- **`check_for_update_notice()`** — Non-blocking check called during `midtown start`. Spawns a background thread and uses `mpsc::recv_timeout(3s)` to enforce a wall-clock deadline independent of the 10s HTTP timeout. Writes the last-check timestamp on both success and failure/timeout to preserve the 1-hour rate limit even when GitHub is unreachable.

**Version comparison** (`is_newer`): Strips pre-release suffixes (e.g., `0.7.0-beta.1` → `0.7.0`) before comparing, so pre-release tags are never considered newer than their stable counterpart.
