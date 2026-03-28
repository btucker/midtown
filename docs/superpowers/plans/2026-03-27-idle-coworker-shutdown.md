# Idle Coworker Shutdown Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a backstop idle coworker detection and shutdown mechanism to the daemon's SessionMonitorTick.

**Architecture:** Pure decision function `decide_idle_shutdowns()` in `rules.rs` takes an `IdleShutdownContext` and returns shutdown decisions. Health wrapper in `health.rs` builds the context from tick state and converts decisions to effects. Wired into the existing `SessionMonitorTick` handler.

**Tech Stack:** Rust, chrono, std::collections

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src/rules.rs` | Modify | Add `IdleShutdownContext`, `IdleShutdownDecision`, `decide_idle_shutdowns()` |
| `src/rules_idle_tests.rs` | Modify | Already has 2 tests; add exclusion tests |
| `src/daemon/health.rs` | Modify | Add `check_and_shutdown_idle_coworkers()` wrapper |
| `src/daemon/events.rs` | Modify | Wire idle check into `SessionMonitorTick` |
| `src/daemon/constants.rs` | Modify | Add `IDLE_COWORKER_MINIMUM_LIFETIME` constant |

---

### Task 1: Add constant and wire test module

**Files:**
- Modify: `src/daemon/constants.rs:61` (near `IDLE_CHECK_INTERVAL`)
- Modify: `src/rules.rs:1058-1061` (test module declarations)

- [ ] **Step 1: Add the minimum lifetime constant**

In `src/daemon/constants.rs`, after `IDLE_CHECK_INTERVAL` (line 61), add:

```rust
/// Minimum age before a coworker is eligible for idle shutdown (90 seconds).
/// Session startup takes 40-60s; 90s provides a 30s buffer to protect
/// coworkers during initialization.
pub(super) const IDLE_COWORKER_MINIMUM_LIFETIME: Duration = Duration::from_secs(90);
```

- [ ] **Step 2: Wire the existing test module into rules.rs**

In `src/rules.rs`, after the `rules_orphan_tests` module declaration (line 1059), add:

```rust
#[path = "rules_idle_tests.rs"]
#[cfg(test)]
mod rules_idle_tests;
```

- [ ] **Step 3: Verify the project compiles (tests will fail — that's expected)**

Run: `cargo check 2>&1 | head -20`

Expected: Compilation errors about `IdleShutdownContext` and `decide_idle_shutdowns` not being found. This confirms the test module is wired and looking for the types we'll define next.

- [ ] **Step 4: Commit**

```bash
git add src/daemon/constants.rs src/rules.rs
git commit -m "chore: add idle shutdown constant and wire test module (!2627)"
```

---

### Task 2: Define IdleShutdownContext and IdleShutdownDecision

**Files:**
- Modify: `src/rules.rs` (after `CoworkerSnapshot` struct, around line 25)

- [ ] **Step 1: Add the context and decision structs**

In `src/rules.rs`, after the `CoworkerSnapshot` struct (line 25), add:

```rust
// ---------------------------------------------------------------------------
// Idle shutdown
// ---------------------------------------------------------------------------

/// Context for idle coworker shutdown decisions.
///
/// Each field is a set of coworker names that should be *excluded* from
/// shutdown. A coworker not in any exclusion set and older than
/// `minimum_lifetime` is eligible for idle shutdown.
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

/// Decision to shut down an idle coworker.
#[derive(Debug, PartialEq)]
pub(crate) struct IdleShutdownDecision {
    pub name: String,
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check 2>&1 | head -20`

Expected: Still fails (test file references `decide_idle_shutdowns` which doesn't exist yet). But the structs should compile cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/rules.rs
git commit -m "feat: define IdleShutdownContext and IdleShutdownDecision structs (!2627)"
```

---

### Task 3: Implement decide_idle_shutdowns and pass existing tests

**Files:**
- Modify: `src/rules.rs` (after the structs from Task 2)
- Test: `src/rules_idle_tests.rs` (existing tests)

- [ ] **Step 1: Implement the decision function**

In `src/rules.rs`, after the `IdleShutdownDecision` struct, add:

```rust
/// Decide which coworkers should be shut down due to idleness.
///
/// This is a backstop — the primary shutdown path is task completion in
/// `dispatch.rs`. This function catches coworkers that slip through:
/// stuck sessions, inconsistent task state, etc.
///
/// A coworker is shut down if:
/// 1. It has been running longer than `minimum_lifetime` (protects startup window)
/// 2. It is not in any exclusion set (no legitimate reason to keep running)
pub(crate) fn decide_idle_shutdowns(ctx: &IdleShutdownContext<'_>) -> Vec<IdleShutdownDecision> {
    let mut decisions = Vec::new();

    for cw in ctx.coworkers {
        let age = ctx
            .now_utc
            .signed_duration_since(cw.started_at)
            .to_std()
            .unwrap_or_default();

        // Protect startup window
        if age < ctx.minimum_lifetime {
            continue;
        }

        let name = &cw.name;

        // Check all exclusion sets — any match means keep running
        if hashset_contains_icase(ctx.channel_lead_names, name)
            || hashset_contains_icase(ctx.busy_coworkers, name)
            || hashset_contains_icase(ctx.coworkers_with_active_tools, name)
            || hashset_contains_icase(ctx.coworkers_with_open_prs, name)
            || hashset_contains_icase(ctx.active_reviewers, name)
            || hashset_contains_icase(ctx.coworkers_with_unblocked_deps, name)
            || hashset_contains_icase(ctx.ci_passed_pr_coworkers, name)
            || hashset_contains_icase(ctx.review_feedback_pr_coworkers, name)
            || hashset_contains_icase(ctx.pending_task_owners, name)
            || hashset_contains_icase(ctx.usage_limited_coworkers, name)
            || hashset_contains_icase(ctx.api_error_coworkers, name)
            || hashset_contains_icase(ctx.auth_error_coworkers, name)
        {
            continue;
        }

        decisions.push(IdleShutdownDecision {
            name: name.clone(),
        });
    }

    decisions
}
```

- [ ] **Step 2: Run the existing tests to verify they pass**

Run: `cargo test rules_idle_tests -- --nocapture 2>&1`

Expected: Both `idle_shutdown_skips_coworker_in_startup_window` and `idle_shutdown_triggers_after_90s_threshold` pass.

- [ ] **Step 3: Commit**

```bash
git add src/rules.rs
git commit -m "feat: implement decide_idle_shutdowns pure decision function (!2627)"
```

---

### Task 4: Add exclusion tests

**Files:**
- Modify: `src/rules_idle_tests.rs`

- [ ] **Step 1: Add a helper function and exclusion tests**

Add to the top of `src/rules_idle_tests.rs` (after the existing imports), a helper to build a default context, then add tests for each exclusion:

```rust
/// Build a default context with one coworker past the minimum lifetime and no exclusions.
/// Callers modify specific fields to test exclusion logic.
fn default_ctx_with_coworker(name: &str) -> (Vec<CoworkerSnapshot>, IdleShutdownContext<'static>) {
    // Leak the sets so we can return references with 'static lifetime in tests.
    // This is fine in tests — they're short-lived.
    let empty: &'static HashSet<String> = Box::leak(Box::new(HashSet::new()));
    let coworkers: &'static [CoworkerSnapshot] = Box::leak(vec![CoworkerSnapshot {
        name: name.to_string(),
        started_at: Utc::now() - chrono::Duration::seconds(120),
        session_id: None,
    }].into_boxed_slice());

    let ctx = IdleShutdownContext {
        coworkers,
        busy_coworkers: empty,
        coworkers_with_open_prs: empty,
        active_reviewers: empty,
        coworkers_with_unblocked_deps: empty,
        ci_passed_pr_coworkers: empty,
        usage_limited_coworkers: empty,
        api_error_coworkers: empty,
        auth_error_coworkers: empty,
        pending_task_owners: empty,
        review_feedback_pr_coworkers: empty,
        coworkers_with_active_tools: empty,
        now_utc: Utc::now(),
        minimum_lifetime: Duration::from_secs(90),
        repo_name: "test-repo",
        channel_lead_names: empty,
    };
    (vec![], ctx)
}

#[test]
fn idle_shutdown_empty_coworkers_returns_empty() {
    let empty: HashSet<String> = HashSet::new();
    let ctx = IdleShutdownContext {
        coworkers: &[],
        busy_coworkers: &empty,
        coworkers_with_open_prs: &empty,
        active_reviewers: &empty,
        coworkers_with_unblocked_deps: &empty,
        ci_passed_pr_coworkers: &empty,
        usage_limited_coworkers: &empty,
        api_error_coworkers: &empty,
        auth_error_coworkers: &empty,
        pending_task_owners: &empty,
        review_feedback_pr_coworkers: &empty,
        coworkers_with_active_tools: &empty,
        now_utc: Utc::now(),
        minimum_lifetime: Duration::from_secs(90),
        repo_name: "test-repo",
        channel_lead_names: &empty,
    };
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_busy_coworker() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let busy: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.busy_coworkers = busy;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_coworker_with_open_pr() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let prs: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.coworkers_with_open_prs = prs;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_active_reviewer() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let reviewers: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.active_reviewers = reviewers;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_unblocked_deps() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let deps: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.coworkers_with_unblocked_deps = deps;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_ci_passed_pr() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let ci: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.ci_passed_pr_coworkers = ci;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_usage_limited() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let limited: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.usage_limited_coworkers = limited;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_api_error() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let errors: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.api_error_coworkers = errors;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_auth_error() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let errors: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.auth_error_coworkers = errors;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_pending_task_owner() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let owners: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.pending_task_owners = owners;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_review_feedback() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let feedback: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.review_feedback_pr_coworkers = feedback;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_active_tools() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let tools: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.coworkers_with_active_tools = tools;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_channel_lead() {
    let (_, mut ctx) = default_ctx_with_coworker("york");
    let leads: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.channel_lead_names = leads;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_multiple_coworkers_only_idle_ones() {
    let empty: &'static HashSet<String> = Box::leak(Box::new(HashSet::new()));
    let busy: &'static HashSet<String> =
        Box::leak(Box::new(HashSet::from(["york".to_string()])));
    let coworkers: &'static [CoworkerSnapshot] = Box::leak(vec![
        CoworkerSnapshot {
            name: "york".to_string(),
            started_at: Utc::now() - chrono::Duration::seconds(120),
            session_id: None,
        },
        CoworkerSnapshot {
            name: "park".to_string(),
            started_at: Utc::now() - chrono::Duration::seconds(120),
            session_id: None,
        },
        CoworkerSnapshot {
            name: "madison".to_string(),
            started_at: Utc::now() - chrono::Duration::seconds(30), // too young
            session_id: None,
        },
    ].into_boxed_slice());

    let ctx = IdleShutdownContext {
        coworkers,
        busy_coworkers: busy, // york is busy
        coworkers_with_open_prs: empty,
        active_reviewers: empty,
        coworkers_with_unblocked_deps: empty,
        ci_passed_pr_coworkers: empty,
        usage_limited_coworkers: empty,
        api_error_coworkers: empty,
        auth_error_coworkers: empty,
        pending_task_owners: empty,
        review_feedback_pr_coworkers: empty,
        coworkers_with_active_tools: empty,
        now_utc: Utc::now(),
        minimum_lifetime: Duration::from_secs(90),
        repo_name: "test-repo",
        channel_lead_names: empty,
    };

    let decisions = decide_idle_shutdowns(&ctx);
    // york is busy → excluded, madison is too young → excluded, only park should be shut down
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].name, "park");
}
```

- [ ] **Step 2: Run all idle tests**

Run: `cargo test rules_idle_tests -- --nocapture 2>&1`

Expected: All 16 tests pass (2 existing + 14 new).

- [ ] **Step 3: Commit**

```bash
git add src/rules_idle_tests.rs
git commit -m "test: add exclusion tests for decide_idle_shutdowns (!2627)"
```

---

### Task 5: Add health wrapper and wire into SessionMonitorTick

**Files:**
- Modify: `src/daemon/health.rs` (add `check_and_shutdown_idle_coworkers`)
- Modify: `src/daemon/events.rs:65-81` (wire into `SessionMonitorTick`)

- [ ] **Step 1: Add the health wrapper function**

In `src/daemon/health.rs`, before the test module declarations at the end of the file (before `#[path = "health_tests.rs"]`), add:

```rust
/// Detect and shut down idle coworkers as a backstop safety net.
///
/// The primary shutdown path is task completion (`stop_sessions_for_completed_tasks`
/// in dispatch.rs). This catches coworkers that slip through: stuck sessions,
/// inconsistent task state, etc.
pub fn check_and_shutdown_idle_coworkers(ps: &DaemonPersistentState) -> Vec<Effect> {
    use crate::rules::{CoworkerSnapshot, IdleShutdownContext, decide_idle_shutdowns};

    // Build coworker snapshots from tick data
    let coworkers: Vec<CoworkerSnapshot> = ps
        .tick_coworker_start_times
        .iter()
        .map(|(name, started_at)| CoworkerSnapshot {
            name: name.clone(),
            started_at: *started_at,
            session_id: None,
        })
        .collect();

    if coworkers.is_empty() {
        return vec![];
    }

    // Collect exclusion sets from tick state
    let open_prs = ps.sessions_with_open_prs();
    let usage_limited = ps.usage_limited_coworkers();
    let api_errors = ps.api_error_coworkers();
    let auth_errors = ps.auth_error_coworkers();
    let channel_leads = ps.channel_lead_names();

    let pending_owners: std::collections::HashSet<String> = ps
        .tick_pending_tasks_with_owners
        .iter()
        .map(|(_, _, owner)| owner.to_lowercase())
        .collect();

    let active_tools: std::collections::HashSet<String> = ps
        .tick_process_health
        .iter()
        .filter(|(_, h)| h.has_pending_tool)
        .map(|(name, _)| name.to_lowercase())
        .collect();

    // For ci_passed, review_feedback, and unblocked_deps we pass empty sets.
    // These require complex derivation (PR CI checks, review comment parsing,
    // dependency graph resolution) that is not yet collected in tick fields.
    // As a backstop, missing these exclusions makes the idle detector slightly
    // more aggressive, which is acceptable — the primary path handles these
    // cases correctly.
    let empty = std::collections::HashSet::new();

    let ctx = IdleShutdownContext {
        coworkers: &coworkers,
        busy_coworkers: &ps.tick_busy_coworkers,
        coworkers_with_open_prs: &open_prs,
        active_reviewers: &ps.tick_active_reviewers,
        coworkers_with_unblocked_deps: &empty,
        ci_passed_pr_coworkers: &empty,
        usage_limited_coworkers: &usage_limited,
        api_error_coworkers: &api_errors,
        auth_error_coworkers: &auth_errors,
        pending_task_owners: &pending_owners,
        review_feedback_pr_coworkers: &empty,
        coworkers_with_active_tools: &active_tools,
        now_utc: chrono::Utc::now(),
        minimum_lifetime: IDLE_COWORKER_MINIMUM_LIFETIME,
        repo_name: &ps.tick_project_name,
        channel_lead_names: &channel_leads,
    };

    let decisions = decide_idle_shutdowns(&ctx);

    if decisions.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();
    for decision in &decisions {
        info!(
            "Idle backstop: shutting down coworker {} (no active work detected)",
            decision.name
        );
        effects.push(Effect::post_to_ops(format!(
            "🔄 Idle backstop: shutting down **{}** — no active work detected",
            decision.name
        )));
        effects.push(Effect::ShutdownCoworker {
            name: decision.name.clone(),
            message: String::new(),
        });
    }

    effects
}
```

- [ ] **Step 2: Wire into SessionMonitorTick**

In `src/daemon/events.rs`, after line 78 (`check_channel_lead_worktree_freshness`), add:

```rust
            effects.extend(super::health::check_and_shutdown_idle_coworkers(&ps));
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1 | tail -5`

Expected: Clean compilation with no errors.

- [ ] **Step 4: Run all tests**

Run: `cargo test 2>&1 | tail -20`

Expected: All tests pass, including the idle tests.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10`

Expected: No warnings.

- [ ] **Step 6: Commit**

```bash
git add src/daemon/health.rs src/daemon/events.rs
git commit -m "feat: add idle coworker shutdown backstop to SessionMonitorTick (!2627)"
```

---

### Task 6: Final verification and cleanup

- [ ] **Step 1: Run the full test suite**

Run: `cargo test 2>&1 | tail -30`

Expected: All tests pass.

- [ ] **Step 2: Run clippy and fmt**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10 && cargo fmt --all -- --check 2>&1`

Expected: No warnings, no formatting issues.

- [ ] **Step 3: Verify the idle tests specifically**

Run: `cargo test idle -- --nocapture 2>&1`

Expected: All idle-related tests pass (unit tests in `rules_idle_tests` + E2E tests in `tests/idle_break_e2e.rs`).
