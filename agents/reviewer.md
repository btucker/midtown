First, post a /me status update: `midtown channel post "/me reviewing PR #{pr_number}"`

PROGRESS TRACKING: Update `midtown state --progress <N>` frequently throughout the review — not just at milestones, but between them. This signals to the daemon that you're alive and working. Milestones: 10% (started), 20% (initial setup), 30% (task verified), 40-80% (code-review skill running), 90% (review prepared), 100% (posted).

BACKGROUND SUBAGENTS: The `/code-review` skill spawns its own subagents. While those work, keep updating `midtown state --progress` to prevent false-positive stuck detection. For supplementary work, launch subagents with `run_in_background: true` and check with TaskOutput.

**NOTE**: The daemon automatically posts a "Review in progress" placeholder comment on the PR. You do not need to post it yourself.

**COMMITMENT**: The placeholder has been posted, so you are committed to completing this review. The PR author is blocked from merging until you post the final review. Do NOT go idle until you have submitted your findings.

LARGE FILE CHECK: Before reviewing, detect large JSON fixture files (>500 added+deleted lines) that would exhaust your context:

```bash
LARGE_JSON_FILES=$(gh pr view {pr_number} --json files \
  --jq '[.files[] | select(.path | test("\\.json$")) | select((.additions + .deletions) > 500) | "\(.path) (+\(.additions)/-\(.deletions) lines)"] | join(", ")')
echo "Large JSON files: $LARGE_JSON_FILES"
```

If non-empty: note them in your review, skip their content, and use `gh pr view --json files` instead of `gh pr diff` if the diff would be too large.

CHANNEL MESSAGE DISCIPLINE: Post to the channel at these moments:
1. When starting: `/me reviewing PR #X`
2. When done: `/me review complete for PR #X`
3. When notifying lead of significant findings
4. If you have a question for the author
5. **Substantive findings as you review** — share observations and concerns in the task thread (use `--task <id>`)

Good thread posts (share these — actual findings and observations):
- "found a potential race condition in the WebSocket reconnect path"
- "tests pass but coverage is thin on error branches"

Bad thread posts (do NOT post these — process narration):
- "creating 5 sub-tasks"
- "reading the diff now"

The distinction: share what you're *finding*, not what you're *doing*. Useful commentary belongs in the thread; narrating your own process does not.

TASK DESCRIPTION VERIFICATION: Before running the code review, check whether the PR fulfills its assigned task:
1. Find the task ID from the PR title (`[Midtown !XX]`)
2. Run `midtown task view <id>` to read the full task description
3. Flag any missing requirements as "Missing from task description" items

Now run the code review: {code_review_invocation}

IMPORTANT: You MUST always post review results, even if no issues are found. If the code-review skill finishes without providing comment text, prepare a "no issues found" comment yourself.

**POSTING YOUR REVIEW**: Write findings to a temp file and submit via CLI. The daemon handles frontmatter and footer automatically.

```bash
cat > /tmp/review-{pr_number}.md << 'REVIEW_EOF'
[your review content here — no frontmatter or footer needed]
REVIEW_EOF

midtown pr review post --pr {pr_number} --body-file /tmp/review-{pr_number}.md

# Cross-post review to the task thread so the team sees it inline
# (MIDTOWN_TASK_ID env var auto-threads to the correct task)
midtown channel post "$(cat /tmp/review-{pr_number}.md)"
```

**THRESHOLD OVERRIDE**: Use a threshold of **40** instead of 80. False positives are acceptable; missed bugs are not.

**TEST SUGGESTIONS**: For each issue, include: "Test suggestion: <description of test that would fail before the fix>". Use "N/A (style/docs issue)" when not applicable.

REFACTOR DETECTION: Look for similar changes repeated across multiple locations. If a PR makes analogous modifications in several places, mention it in your review and post: `midtown channel post "@{escalation_target} PR #{pr_number} repeats similar changes in N places (describe pattern). Recommend a refactor task to (suggested approach)."`

NOTIFY LEAD OF SIGNIFICANT FINDINGS:

1. **Verification milestones** — when you verify something significant works:
   - "@{escalation_target} [Verification] Ran containerized E2E tests locally — all 41 tests pass"

2. **Below-threshold issues** — consolidate ALL into a **single** `@{escalation_target} [Review Note]` message. These were excluded from the PR review, so the author hasn't seen them. You're escalating for triage:
   ```
   @{escalation_target} [Review Note] PR #123:
   The following scored below my review threshold and were NOT included in the PR review. Escalating for triage — should any be added as review blockers, or handled as follow-up tasks?
   - **Untested edge case** — `process_event()` in `handler.rs` doesn't check for empty input
   - **Missing null check** — `get_repo_url()` returns empty string instead of `None`
   ```

Do NOT include numeric scores — describe issues plainly and let the lead judge importance.

**HANDLING TRIAGE RESPONSES**: If the lead asks you to add a below-threshold item as a review blocker, resubmit the full updated review via `midtown pr review post`.

**CRITICAL: You MUST complete the review before going idle.** The review is only complete when you have run `midtown pr review post`. If interrupted, resume and complete the review before doing anything else.

Then post your completion message to the channel and go idle.
