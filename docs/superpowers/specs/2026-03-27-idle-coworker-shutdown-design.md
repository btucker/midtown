# Idle Coworker Detection and Shutdown

**Task:** !2627
**Date:** 2026-03-27

## Summary

Add daemon-side idle coworker detection as a backstop safety net. The primary mechanism for stopping done coworkers is task completion (`stop_sessions_for_completed_tasks()` in dispatch.rs). This feature catches cases where task completion doesn't fire — stuck coworkers, inconsistent task state, etc.

## Architecture

Follows the established decision function + health wrapper pattern:

```
SessionMonitorTick (events.rs)
  → health::check_and_shutdown_idle_coworkers(ps)
    → rules::decide_idle_shutdowns(ctx)
      → Vec<IdleShutdownDecision>
    → convert to Vec<Effect>
```

## Components

### 1. `IdleShutdownContext` (rules.rs)

Immutable context struct passed to the pure decision function:

```rust
pub(crate) struct IdleShutdownContext<'a> {
    pub coworkers: &'a [CoworkerSnapshot],
    pub busy_coworkers: &'a HashSet<String>,
    pub coworkers_with_open_prs: &'a HashSet<String>,
    pub active_reviewers: &'a HashSet<String>,
    pub coworkers_with_unblocked_deps: &'a HashSet<String>,
    pub ci_passed_pr_coworkers: &'a HashSet<String>,
    pub usage_limited_coworkers: &'a HashSet<String>,
    pub api_error_coworkers: &'a HashSet<String>,
    pub auth_error_coworkers: &'a HashSet<String>,
    pub pending_task_owners: &'a HashSet<String>,
    pub review_feedback_pr_coworkers: &'a HashSet<String>,
    pub coworkers_with_active_tools: &'a HashSet<String>,
    pub now_utc: DateTime<Utc>,
    pub minimum_lifetime: Duration,
    pub repo_name: &'a str,
    pub channel_lead_names: &'a HashSet<String>,
}
```

### 2. `IdleShutdownDecision` (rules.rs)

```rust
#[derive(Debug, PartialEq)]
pub(crate) struct IdleShutdownDecision {
    pub name: String,
    pub reason: String,
}
```

### 3. `decide_idle_shutdowns` (rules.rs)

Pure decision function. For each coworker:

1. Skip if age < `minimum_lifetime` (90s) — protects startup window
2. Skip if in any exclusion set (see table below)
3. Return `IdleShutdownDecision` for remaining coworkers

### 4. `check_and_shutdown_idle_coworkers` (health.rs)

Effect wrapper function:

- Builds `IdleShutdownContext` from `DaemonPersistentState` tick fields
- Calls `decide_idle_shutdowns`
- Converts each decision to `Effect::post_to_ops(reason)` + `Effect::ShutdownCoworker { name, message: "" }`

### 5. Wire into SessionMonitorTick (events.rs)

Single line addition after existing health checks:

```rust
effects.extend(super::health::check_and_shutdown_idle_coworkers(&ps));
```

### 6. Constant (constants.rs)

```rust
pub(super) const IDLE_COWORKER_MINIMUM_LIFETIME: Duration = Duration::from_secs(90);
```

## Exclusion Logic

Any match prevents shutdown. Checked in this order:

| Exclusion Set | Rationale |
|---|---|
| `channel_lead_names` | Channel leads are never idle-killed |
| `busy_coworkers` | Has in-progress task |
| `coworkers_with_active_tools` | Tool execution in progress |
| `coworkers_with_open_prs` | Waiting for review/CI |
| `active_reviewers` | Performing code review |
| `coworkers_with_unblocked_deps` | Ready to work on dependent task |
| `ci_passed_pr_coworkers` | PR passed CI, may need merge action |
| `review_feedback_pr_coworkers` | Has review feedback to address |
| `pending_task_owners` | Has pending (not yet started) task |
| `usage_limited_coworkers` | Waiting for rate limit to clear |
| `api_error_coworkers` | Experiencing API errors |
| `auth_error_coworkers` | Needs auth intervention |

## Testing

### Existing tests (rules_idle_tests.rs)

- `idle_shutdown_skips_coworker_in_startup_window` — 75s old coworker protected by 90s threshold
- `idle_shutdown_triggers_after_90s_threshold` — 95s old coworker with no exclusions is shut down

### New tests to add (rules_idle_tests.rs)

- Each exclusion set individually prevents shutdown (12 tests)
- Multiple coworkers: only non-excluded ones get shutdown decisions
- Empty coworkers list returns empty decisions

## Data Sources for Exclusion Sets

The health wrapper (`check_and_shutdown_idle_coworkers`) builds the context from these sources:

| Exclusion Set | Source |
|---|---|
| `busy_coworkers` | `ps.tick_busy_coworkers` (tick field) |
| `active_reviewers` | `ps.tick_active_reviewers` (tick field) |
| `coworkers_with_open_prs` | `ps.sessions_with_open_prs()` (method) |
| `usage_limited_coworkers` | `ps.usage_limited_coworkers()` (method) |
| `api_error_coworkers` | `ps.api_error_coworkers()` (method) |
| `auth_error_coworkers` | `ps.auth_error_coworkers()` (method) |
| `channel_lead_names` | `ps.channel_lead_names()` (method) |
| `pending_task_owners` | `ps.tick_pending_tasks_with_owners` → extract owner names |
| `coworkers_with_active_tools` | `ps.tick_process_health` → filter `has_pending_tool` |
| `ci_passed_pr_coworkers` | `ps.tick_open_prs` + `helpers::all_ci_checks_passed()` → map to session names |
| `coworkers_with_unblocked_deps` | Derive from `ps.tick_blocks_map` + completed tasks |
| `review_feedback_pr_coworkers` | `ps.tick_open_prs` + review feedback helpers → map to session names |
| `coworkers` | `ps.tick_coworker_start_times` → build `CoworkerSnapshot` vec |

## Non-goals

- **No new state tracking** — uses existing `tick_*` fields from `DaemonPersistentState`
- **No parallel shutdown path** — reuses `Effect::ShutdownCoworker`
- **No tool activity monitoring** — relies on existing busy/active categorization in tick collection
- **No cooldown tracking** — the coworker is stopped, so it won't appear in the next tick
- **No duplication of task-completion path** — this is a backstop only
