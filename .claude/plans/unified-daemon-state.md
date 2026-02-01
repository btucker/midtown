# Design: Unified Daemon-Owned State Model

**Task**: #6 — Design unified daemon-owned state model replacing scattered state sources
**Author**: amsterdam
**Date**: 2026-02-01

---

## Problem Statement

The daemon's state is currently spread across 6+ distinct sources with different persistence strategies, access patterns, and lifecycles:

| # | Source | Type | Persistent? | Written By | Read By |
|---|--------|------|-------------|------------|---------|
| 1 | `CoworkerStatus` (coworker.rs) | In-memory enum | No | CoworkerManager | snapshot, effects |
| 2 | `CoworkerPhase` / `CoworkerLifecycle` (rules.rs) | In-memory HashMap | No | rules/effects | rules via snapshot |
| 3 | `state.json` per coworker (coworker_state.rs) | Persistent file | Yes | Coworker CLI (`midtown state`) | daemon (tmux tab update), snapshot |
| 4 | Task files `~/.claude/tasks/` (tasks.rs) | Persistent files | Yes | Claude Code task API | snapshot, rules |
| 5 | `github-state.json` (github_state.rs) | Persistent file | Yes | Daemon effects | snapshot, rules |
| 6 | Tmux pane scraping (tmux.rs) | Ephemeral | No | Tmux (capture-pane) | snapshot |

Additional in-memory trackers in `DaemonState`:
- `PrCoworkerCache` (open/merged/ci-passed PR owners)
- `pr_break_sessions` (saved session IDs for resume)
- `coworker_pane_hashes` (stuck detection)
- `zombie_respawn_counts`
- `stuck_tracker`
- `cooldowns` (CooldownTracker)
- `lead_typing` state
- `usage_limit_nudge_at`

### Pain Points

1. **No single truth for coworker state** — Process status is in `CoworkerManager`, workflow phase is in `state.json` (file), daemon-observed health is in `CoworkerPhase`/pane scraping. These can disagree.
2. **GitHub state is a separate persistent file** — `github-state.json` duplicates the persistence pattern of the main daemon, but lives in its own silo with its own load/save cycle.
3. **`state.json` is a file-based RPC** — Coworkers write a file, daemon reads it. This is fragile (stale files after crashes) and has no acknowledgment path.
4. **WorldSnapshot is a flat bag of 30+ fields** — It re-derives sets from the scattered sources every tick, making it hard to see what state actually matters for which decisions.
5. **DaemonState has 20+ fields** — Many are single-purpose trackers that could be folded into a per-coworker or per-project model.

---

## Design Goals

1. **Daemon is single authority** — One canonical state model replaces `github-state.json`, per-coworker `state.json`, and in-memory `CoworkerPhase`/`CoworkerLifecycle`.
2. **Coworkers report via RPC** — `midtown state` writes to the daemon via the existing Unix socket (RPC), not a file. Daemon records and acknowledges.
3. **Pane scraping as safety net** — Remains for health checks (zombie, stuck, crash detection) but is no longer the primary source of workflow state.
4. **Unified per-coworker model** — Process status, workflow phase, health, task, PR, and review state are fields on a single `CoworkerRecord`.
5. **Single persistent file** — All daemon state persists to one `daemon-state.json` file, replacing `github-state.json` and per-coworker `state.json` files.
6. **Preserve pure/impure boundary** — The existing `rules.rs` → `Effect` → `effects.rs` architecture stays. `WorldSnapshot` continues as the read interface for pure functions, but is now derived from the unified model instead of scattered sources.

---

## Proposed State Model

### Core Types

```rust
/// The unified daemon state model. Single source of truth.
/// Persisted to ~/.midtown/projects/<repo>/daemon-state.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStateModel {
    /// Per-coworker state, keyed by coworker name (lowercase).
    pub coworkers: HashMap<String, CoworkerRecord>,

    /// GitHub integration state (collapsed from github_state.rs).
    pub github: GitHubIntegration,

    /// Metadata: schema version, last persist time.
    pub meta: StateMeta,
}

/// Everything the daemon knows about a single coworker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoworkerRecord {
    // ── Identity ──
    pub name: String,
    pub working_dir: String,

    // ── Process state (daemon-observed) ──
    /// Is the tmux window alive? Set by CoworkerManager.
    pub process_status: ProcessStatus,  // Starting | Running | Stopping | Gone
    pub started_at: DateTime<Utc>,
    pub session_id: Option<String>,     // Claude Code session UUID
    pub isolated_tasks: bool,           // true for review coworkers

    // ── Workflow state (coworker-reported via RPC) ──
    /// Last phase reported by the coworker via `midtown state`.
    pub workflow_phase: Option<WorkflowPhase>,
    /// Task the coworker says they're working on.
    pub reported_task_id: Option<u32>,
    /// When the coworker last reported state.
    pub last_state_report: Option<DateTime<Utc>>,

    // ── Daemon-observed health state ──
    /// Health phase determined by pane scraping / daemon heuristics.
    pub health: HealthStatus,
    /// Hash of last pane content (for stuck detection).
    pub last_pane_hash: Option<u64>,
    /// When pane content last changed.
    pub last_pane_change: Option<DateTime<Utc>>,
    /// Zombie respawn attempts.
    pub zombie_respawn_count: u32,

    // ── Lifecycle phase (daemon's nudge/idle state machine) ──
    /// The daemon's lifecycle tracking for nudge decisions.
    /// Not persisted — reconstructed on startup from process_status.
    #[serde(skip)]
    pub lifecycle: Option<CoworkerPhase>,
    /// When the coworker last posted to the channel.
    #[serde(skip)]
    pub last_channel_activity: Option<Instant>,

    // ── PR state ──
    /// Does this coworker have an open PR?
    pub has_open_pr: bool,
    /// Has this coworker's PR been merged recently?
    pub has_merged_pr: bool,
    /// Are all CI checks passing on their open PR?
    pub ci_passed: bool,
    /// Saved session ID for PR break resume.
    pub pr_break_session_id: Option<String>,

    // ── Review state ──
    /// PR number this coworker is assigned to review (if any).
    pub reviewing_pr: Option<u64>,
    /// When the review was assigned.
    pub review_assigned_at: Option<DateTime<Utc>>,
}

/// Process status as observed by the daemon (tmux window existence).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProcessStatus {
    Starting,
    Running,
    Stopping,
    Gone, // window disappeared without clean shutdown
}

/// Health status derived from pane scraping (daemon watchdog).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HealthStatus {
    /// Normal operation — pane has output, content is changing.
    Healthy,
    /// Pane is blank — potential zombie.
    BlankPane,
    /// Pane content hasn't changed for an extended period — potentially stuck.
    Stuck { since: DateTime<Utc> },
    /// Session shows interruption marker.
    Interrupted,
    /// Session is blocked on an interactive prompt.
    Prompted { fingerprint: String },
}

/// GitHub integration state (replaces github_state.rs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitHubIntegration {
    /// PR reviewer assignments, keyed by PR number.
    pub pr_reviewers: HashMap<u64, PrReviewerAssignment>,
    /// PRs with confirmed Claude review (monotonic cache).
    pub reviewed_prs: HashSet<u64>,
    /// Pending reviewer spawns waiting for delay to expire.
    pub pending_review_spawns: Vec<PendingReviewSpawn>,
}

/// Schema version and bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMeta {
    pub schema_version: u32,  // start at 1
    pub last_persisted: DateTime<Utc>,
}
```

### Key Design Decisions

#### 1. CoworkerRecord Merges Five Concerns

Instead of five separate data structures (CoworkerStatus, CoworkerLifecycle, CoworkerStateReport, pane hashes, PR cache), there's one `CoworkerRecord` per coworker. This makes it obvious what the daemon knows about each coworker at any point.

**What moves in**:
- `CoworkerStatus` → `process_status` field
- `CoworkerLifecycle.phase` → `lifecycle` field (transient, not persisted)
- `CoworkerLifecycle.last_activity` → `last_channel_activity` (transient)
- `CoworkerStateReport` (from state.json) → `workflow_phase` + `reported_task_id` + `last_state_report`
- `PrCoworkerCache` entries → `has_open_pr` + `has_merged_pr` + `ci_passed`
- `pr_break_sessions` → `pr_break_session_id`
- `coworker_pane_hashes` → `last_pane_hash` + `last_pane_change`
- `zombie_respawn_counts` → `zombie_respawn_count`

#### 2. GitHub State Collapses In

`GitHubIntegration` is structurally identical to the current `GitHubState` but lives inside `DaemonStateModel` rather than in a separate file. Same fields, same logic — just unified persistence.

Additionally, the per-coworker `reviewing_pr` field on `CoworkerRecord` provides a fast path for "is this coworker a reviewer?" checks without scanning the `pr_reviewers` map.

#### 3. `midtown state` Becomes an RPC Call

Currently:
```
Coworker → writes state.json file → Daemon reads file on /me message
```

Proposed:
```
Coworker → midtown state (RPC over Unix socket) → Daemon updates CoworkerRecord in-memory + persists
```

The existing `midtown` CLI already has a Unix socket RPC path for other commands. Adding a `ReportState { phase, task_id }` request type is straightforward.

**Fallback**: If RPC fails (daemon unreachable), the coworker still writes `state.json` as a fallback. The daemon can read this on startup to recover state.

#### 4. Persistence Strategy

**Single file**: `~/.midtown/projects/<repo>/daemon-state.json`

**Write frequency**: Debounced — write at most once per 5 seconds, triggered by state mutations. Use the existing atomic temp+rename pattern.

**What's NOT persisted** (transient, `#[serde(skip)]`):
- `lifecycle` (CoworkerPhase for nudge decisions) — reconstructed from process observation
- `last_channel_activity` (Instant) — only meaningful in current daemon session
- Anything derived from pane scraping — re-collected each tick

**What IS persisted**:
- Workflow phase, task, PR state, review assignments — survives daemon restarts
- GitHub integration (reviewer assignments, reviewed PRs, pending spawns)
- Process status snapshots — helps detect stale state on restart

#### 5. WorldSnapshot Derivation

`collect_world_snapshot()` becomes simpler — it reads primarily from `DaemonStateModel` instead of reaching into 6+ sources:

```rust
pub async fn collect_world_snapshot(model: &DaemonStateModel, ...) -> WorldSnapshot {
    // Most fields are direct derivations from model.coworkers
    let active_coworkers: Vec<_> = model.coworkers.values()
        .filter(|c| c.process_status != ProcessStatus::Gone)
        .collect();

    let busy_coworkers: HashSet<_> = model.coworkers.values()
        .filter(|c| c.reported_task_id.is_some())
        .map(|c| c.name.clone())
        .collect();

    // Pane contents still scraped from tmux (ephemeral, not in model)
    let pane_contents = scrape_panes(&active_coworkers, session_name);

    // Tasks still read from Claude Code's task storage (external system)
    let all_tasks = crate::tasks::read_tasks();

    // ... rest derived from model fields
}
```

**Tasks remain external**: Claude Code owns task files (`~/.claude/tasks/`). The daemon reads them but doesn't try to own them. This is correct — tasks are a shared contract between the lead and coworkers via Claude Code's native API.

---

## Migration Path

### Phase 1: Introduce `DaemonStateModel` + `CoworkerRecord` (new module)

- Create `src/daemon/state_model.rs` with the types above
- Add `DaemonStateModel` as a field on `DaemonState`
- Load from `daemon-state.json` on startup, fallback to defaults
- Persist on mutation (debounced)
- **No behavior changes** — existing scattered state continues to work

### Phase 2: Migrate GitHub state into the model

- Move `GitHubState` fields into `DaemonStateModel.github`
- Remove `github-state.json` reads/writes
- Update `github_state.rs` to delegate to the unified model
- Delete `github_state.rs` once fully migrated

### Phase 3: Add RPC path for `midtown state`

- Add `ReportState` variant to the RPC request enum
- Daemon handler updates `CoworkerRecord.workflow_phase` + `reported_task_id`
- `midtown state` CLI sends RPC first, falls back to file write
- Daemon still reads `state.json` as fallback (startup recovery)

### Phase 4: Collapse per-coworker state into `CoworkerRecord`

- Move `PrCoworkerCache` fields → per-coworker `has_open_pr`, `has_merged_pr`, `ci_passed`
- Move `pr_break_sessions` → per-coworker `pr_break_session_id`
- Move `coworker_pane_hashes` → per-coworker `last_pane_hash`, `last_pane_change`
- Move `zombie_respawn_counts` → per-coworker `zombie_respawn_count`
- Slim down `DaemonState` by removing migrated fields

### Phase 5: Simplify WorldSnapshot

- Derive snapshot fields from `DaemonStateModel` instead of scattered sources
- Remove redundant HashSet derivations that are now just views of the model
- Potentially flatten the snapshot (it may shrink significantly)

### Phase 6: Cleanup

- Remove `coworker_state.rs` (file-based state reporting) once RPC is stable
- Remove `github_state.rs` (fully merged into model)
- Remove per-coworker `state.json` writes from hooks (or keep as emergency fallback)
- Update `CLAUDE.md` to document the new architecture

---

## What Stays Unchanged

- **Task storage** — Claude Code owns `~/.claude/tasks/`. The daemon reads but doesn't own.
- **Channel log** — Append-only JSONL at `channel.jsonl`. Remains the team communication record.
- **Tmux pane scraping** — Continues as health watchdog. Feeds into `CoworkerRecord.health`.
- **Pure/impure boundary** — `rules.rs` still returns `Vec<Effect>`, `effects.rs` still executes them. The model is the store that both read from and write to.
- **CoworkerManager** — Still manages tmux windows. But the authoritative process status lives in `CoworkerRecord.process_status`, synchronized from CoworkerManager operations.

---

## Open Questions

1. **Should `CoworkerRecord` own the lifecycle phase, or should it stay in a separate map?** The lifecycle phase (`Idle { since }`, `Interrupted { since }`, `Prompted { fingerprint }`) uses `std::time::Instant` which is not serializable. We could store it on `CoworkerRecord` with `#[serde(skip)]` (proposed above) or keep the separate `coworker_lifecycles` HashMap. The former is cleaner conceptually; the latter avoids mixing transient and persistent fields.

2. **Debounce interval for persistence** — 5 seconds is proposed. Too frequent = disk thrashing; too infrequent = data loss on crash. The current `github-state.json` saves on every mutation, which works fine for the current write frequency (~1/minute). The unified model will see more mutations (every pane scrape updates health status), so debouncing is important.

3. **Schema migration strategy** — When `daemon-state.json` schema evolves, do we version+migrate or just add `#[serde(default)]` to new fields? Given the state is reconstructible (non-persistent fields are derived, and the daemon can re-observe everything), `#[serde(default)]` on new fields is likely sufficient. The `schema_version` field enables breaking changes if needed.

---

## Relationship to Other Tasks

- **Task #5** (Tighten pure/impure boundary): Complementary. That task cleans up `rules.rs` decision functions; this task simplifies what they read from. The unified model makes the snapshot simpler to construct, which helps the pure functions.
- **Task #7** (Break up daemon/mod.rs): The new `state_model.rs` module is a natural extraction point. The unified model reduces the field count on `DaemonState`, making mod.rs smaller.
