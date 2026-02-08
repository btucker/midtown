# Notification Path Duplication Audit

## Executive Summary

Audited the daemon's notification/nudge system for duplicated paths that produce
inconsistent output. Found **6 duplications** across webhook, polling, and dispatch
paths. The core deduplication mechanism (`PrIssueTracker` cooldowns) prevents
actual double-delivery, but message formats diverge and one structural pattern
(webhook handlers bypassing the Effect system) creates maintenance burden.

---

## Finding 1: Webhook Handlers Bypass the Effect Pipeline

**Severity: Structural / High maintenance cost**

The polling path uses the pure Effect pipeline (decision in `rules.rs` →
`Vec<Effect>` → `execute_effects()`), but the three webhook handlers execute
side effects inline:

| Handler | Location | Inline effects |
|---------|----------|----------------|
| `handle_pr_comment_nudge` | `pr.rs:2077-2256` | `state.coworkers.nudge()`, `state.spawn_coworker()`, `state.send_and_broadcast()` |
| `handle_webhook_ci_failure` | `pr.rs:2454-2632` | Same pattern |
| `handle_webhook_review_state_change` | `pr.rs:2263-2452` | Same pattern |

**Divergence**: Both paths call the same decision function
(`decide_pr_issue_action_with_handoff`), but:
- Polling: Returns `Vec<Effect>` via `pr_action_to_effects()`, executed by the effect executor
- Webhook: Matches on `PrAction` variants and executes directly (nudge, spawn, channel post)

**Impact**: Changes to spawn logic, channel message format, or broadcast behavior
must be updated in two places. For example, the polling `SpawnOwner` path in
`pr_action_to_effects()` (line 675-741) includes `ClearPrBreakSession` in
on_success callbacks, while the webhook `SpawnOwner` path (e.g., line 2181-2210)
does the session cleanup inline — same logic, different code.

**Note**: This gap is already acknowledged in `events.rs:22-24`, which documents
the plan to add webhook event variants to `DaemonEvent`, converting the remaining
inline side effects to the evaluate/execute pattern. This finding recommends
prioritizing that existing plan as the single highest-impact consolidation.

**Recommendation**: Accelerate the planned webhook-to-Effect migration documented
in `events.rs`. Refactor webhook handlers to return `Vec<Effect>` and share
`pr_action_to_effects()` / `comment_action_to_effects()` with the polling path.

---

## Finding 2: CI Failure Message Format Inconsistency

**Severity: Low / Cosmetic**

**Path A — Webhook** (`pr.rs:2486-2489`):
```
"PR #{pr_number} — CI check '{check_name}' failed: please investigate"
```

**Path B — Polling** (`pr.rs:421-427`):
```
"PR #{pr_number} ({title}) - {issue_type}: {action}"
→ "PR #42 (Add feature) - CI Failed: please investigate"
```

**Differences**:
1. Webhook uses em dash (`—`), polling uses hyphen (` - `)
2. Webhook includes specific check name, polling includes PR title
3. Webhook uses lowercase "failed", polling uses title case "CI Failed"

**Deduplication**: Shared `PrIssueTracker` cooldown on `(pr_number, CiFailed)`
prevents both from firing, so users see only one. But *which* format they see
depends on whether the webhook or poll fires first.

**Recommendation**: Extract a `format_ci_failure_nudge()` helper to
`daemon_messages.rs` that both paths call. Include both check name and PR title
for maximum context.

---

## Finding 3: Review Comment Message Format Inconsistency

**Severity: Low / Cosmetic**

**Path A — Webhook** (`pr.rs:2106-2108`):
```
"Your PR #{pr_number} has review feedback from {actor}. Please address it and merge if appropriate."
```

**Path B — Polling** (`pr.rs:1184-1188`):
```
"Your PR #{pr_number} ({title}) has new review comments — please address feedback."
```

**Differences**:
1. Webhook includes actor name, polling uses generic "new review comments"
2. Webhook says "merge if appropriate", polling just says "address feedback"
3. Webhook omits PR title, polling includes it

**Additionally**, the polling spawn channel message uses `called_in_review_feedback()`
from `daemon_messages.rs` while the webhook spawn posts an ad-hoc message:
```rust
// Webhook (pr.rs:2193-2196) — ad-hoc, no personality support
"Called in {} to address review feedback on PR #{}"

// Polling (pr.rs:1263-1267) via comment_action_to_effects → daemon_messages
called_in_review_feedback(name, pr_number, personality)
→ "🚀 Called in {name} to address review feedback on PR #{pr}"
```

The webhook path's channel message lacks the emoji prefix and personality
variants that the polling path provides.

**Recommendation**: Both paths should use `daemon_messages::called_in_review_feedback()`
for channel messages, and share a `format_review_comment_nudge()` helper for nudge text.

---

## Finding 4: `called_in_pending_task` Missing Task Subject

**Severity: Medium / Information loss**

**`daemon_messages.rs:77-88`** — `called_in_pending_task()` does NOT accept a subject:
```rust
"🚀 Called in coworker {name} for pending task !{task}"
```

**`daemon_messages.rs:91-108`** — `called_in_assigned_task()` DOES accept a subject:
```rust
"🚀 Called in coworker {name} for assigned task !{task}: {subject}"
```

**Impact**: When the daemon spawns a coworker for a pending task with an existing
owner (`dispatch.rs:697-704`), the team channel message omits the subject. The
coworker's initial prompt (`dispatch.rs:679-681`) includes the subject, but the
team has no visibility into what task was assigned.

**Recommendation**: Add a `subject` parameter to `called_in_pending_task()`,
matching `called_in_assigned_task()`.

---

## Finding 5: Recovery/Restart Channel Messages Missing Task Subject

**Severity: Medium / Information loss**

Four recovery paths include full task subject in the coworker prompt but omit it
from the team-visible channel message:

| Path | Coworker prompt | Channel message |
|------|----------------|-----------------|
| Orphan recovery (`dispatch.rs:76-78`) | `"task !{id}: {subject}"` | `"♻️ Recovered coworker {name} for orphaned task !{id}"` (line 106-108) |
| Discovered coworker (`dispatch.rs:187-189`) | `"Resume task !{id}: {subject}"` | `"♻️ Nudged discovered coworker {name} to resume task !{id}"` (line 204-206) |
| Stuck restart (`health.rs:442-444`) | `"task !{id}: {subject}"` | `"🔄 Restarted stuck coworker {name} ... resuming task !{id}"` (line 462-465) |
| Pending task spawn (`dispatch.rs:679-681`) | `"task !{id}: {subject}"` | `called_in_pending_task(name, id)` — no subject (line 699-702) |

**Recommendation**: Include `{subject}` in all recovery/restart channel messages
for consistency with `called_in_assigned_task()`.

---

## Finding 6: Silent Coworker — Dual Nudge + System Message

**Severity: Low / By design but worth noting**

When a silent coworker is first detected (`pr.rs:997-1017`), TWO effects are
produced:
1. `Effect::NudgeCoworker` — direct tmux nudge to the coworker (line 1005-1008)
2. `Effect::PostSystemMessage` — channel post with "⚠️ Nudging {name} — silent..." (line 1010-1017)

This is intentional (the nudge is private, the channel post is for team visibility),
but contrasts with the stuck PR patterns (no-review, unresolved-feedback, merge-ready)
which only use `PostSystemMessage` and rely on `@lead` mention routing.

On escalation (second nudge), the silent coworker path switches to the
`stuck_nudge_effects()` pattern (system message only, line 1028), aligning with
the other stuck types.

**No action needed** — the dual approach for first nudge is correct (direct nudge
to coworker + team visibility). Documenting for completeness.

---

## Finding 7: Pending Task Nudge — No Channel Message

**Severity: Low**

When a pending task's existing owner is already running and gets nudged
(`dispatch.rs:661-668`), only a `NudgeCoworkerWithCallbacks` is produced with a
`RecordCooldown` on success. No `PostToChannel` effect is included.

Compare with the unowned task assignment path (`dispatch.rs:911-918`) which posts
a channel message on nudge success.

**Impact**: Team has no visibility when an already-running coworker is nudged about
their pending task.

**Recommendation**: Add `PostToChannel` to the `on_success` callbacks, matching the
unowned task pattern.

---

## Summary of Recommendations

### High Priority

1. **Accelerate planned webhook-to-Effect migration** (Finding 1)
   - `handle_pr_comment_nudge`, `handle_webhook_ci_failure`, `handle_webhook_review_state_change`
   - Already documented as future work in `events.rs:22-24`
   - Should return `Vec<Effect>` and reuse `pr_action_to_effects()` / `comment_action_to_effects()`
   - Eliminates the primary source of format divergence

### Medium Priority

2. **Add task subject to `called_in_pending_task()`** (Finding 4)
   - `daemon_messages.rs:77-88` — add `subject` parameter
   - Update caller in `dispatch.rs:699-702`

3. **Add task subject to recovery channel messages** (Finding 5)
   - `dispatch.rs:106-108` (orphan recovery)
   - `dispatch.rs:204-206` (discovered coworker)
   - `health.rs:462-465` (stuck restart)

### Low Priority

4. **Standardize nudge message formats** (Findings 2, 3)
   - Extract `format_ci_failure_nudge()` and `format_review_comment_nudge()` to `daemon_messages.rs`
   - Both webhook and polling paths call the same formatter

5. **Add channel message to pending task nudge** (Finding 7)
   - `dispatch.rs:661-668` — add `PostToChannel` to `on_success` callbacks

---

## Architecture Note

The `PrIssueTracker` cooldown mechanism successfully prevents actual duplicate
notifications across webhook and polling paths for all PR-related events. The
issues identified here are about **format consistency** and **code maintainability**,
not functional bugs. The highest-value change is consolidating the webhook handlers
onto the Effect pipeline (Finding 1), which would automatically resolve Findings 2
and 3 as a side effect.
