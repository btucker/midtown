# Fallback Review Detection: Complete Review Task on Idle + Any PR Comment

**Date:** 2026-03-28
**Task:** !2649

## Problem

The review auto-complete mechanism (commit 09379bb) relies solely on midtown session frontmatter (`<!-- midtown session:X type:review -->`) to detect completed reviews via `text_contains_review_signature()`. When a reviewer (e.g., Codex) posts a PR comment without this frontmatter, the daemon can't detect the review. The task stays `in_progress` forever.

## Solution: Idle-Path Fallback

Add a fallback check in the `rpc_coworker.rs` idle handler. When a reviewer session reports idle AND `is_pr_reviewed()` returns false:

1. Check if the reviewer session has posted **any** comment on the PR (not just frontmatter-tagged ones)
2. If yes → treat as review complete (complete the task, shut down the reviewer)
3. If no → nudge to post review (existing behavior)

### Why idle-path only

The task description specifies "posted any PR comment AND goes idle" — the idle signal is the key qualifier. The reviewer chose to stop working, suggesting they believe they're done. This is the narrowest, safest check: no risk of false positives from "review in progress" placeholder comments on the polling path.

## Detection Logic

New function: `reviewer_session_has_any_pr_comment(pr_number, session_id) -> bool`

1. Fetch PR comments via `gh pr view <pr> --json comments`
2. Check if any comment's body contains the session's frontmatter (`<!-- midtown session:<id> -->`) — note: checking for session frontmatter WITHOUT `type:review`, so any comment from the session counts
3. If session frontmatter isn't found, fall back to checking if any comment contains a `<!-- midtown: <name> -->` legacy frontmatter matching the reviewer name

This approach is deliberately conservative: it only matches comments that are attributable to the assigned reviewer session, not arbitrary comments from other users.

## Integration Point

In `rpc_coworker.rs`, the idle handler currently has:

```
if reviewer_pr is Some AND !is_pr_reviewed(pr) → nudge
if reviewer_pr is Some AND is_pr_reviewed(pr) → complete task
```

After the change:

```
if reviewer_pr is Some AND !is_pr_reviewed(pr):
    if reviewer_session_has_any_pr_comment(pr, session_id) → complete task (fallback)
    else → nudge
if reviewer_pr is Some AND is_pr_reviewed(pr) → complete task (existing)
```

## Files to Modify

- `src/daemon/pr.rs`: Add `reviewer_session_has_any_pr_comment()` (pure logic) + `check_reviewer_has_any_pr_comment()` (subprocess wrapper)
- `src/daemon/rpc_coworker.rs`: Integrate fallback check in idle handler
- `src/daemon/pr_tests.rs`: Unit tests for the new detection function

## Testing

- Unit test: `reviewer_session_has_any_pr_comment` with mock JSON — comment with session frontmatter (no type:review) → true
- Unit test: comment with different session ID → false
- Unit test: no comments → false
- Unit test: comment with legacy `<!-- midtown: name -->` frontmatter → true
- Integration: idle handler completes task when fallback detects comment
