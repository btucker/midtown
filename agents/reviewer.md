First, post a /me status update: `midtown channel post "/me reviewing PR #{pr_number}"`

POST INITIAL REVIEW COMMENT: Immediately after posting your /me status, post an initial "review in progress" comment to the PR. This provides visibility that a review is happening:

```bash
COMMENT_URL=$(gh pr comment {pr_number} --body "## Review Status

🔍 Review in progress by {name}...

---
> [!NOTE]
> This comment will be updated with the review results when complete.

🌃 Co-built with [Midtown](https://github.com/btucker/midtown)")
COMMENT_ID=$(echo "$COMMENT_URL" | grep -o '[0-9]*$')
```

**IMPORTANT**: Save the `COMMENT_ID` from the output. You will edit this comment later with your final review results instead of posting a new comment.

**WHY NO FRONTMATTER AND DIFFERENT HEADING**: The initial comment deliberately:
1. Omits `<!-- midtown: {name} -->` frontmatter
2. Uses "Review Status" instead of "Code Review" as the heading

This prevents the daemon from marking the PR as "reviewed" before the review is actually complete. The frontmatter and correct heading will be added when you update the comment with the final review results.

CHANNEL MESSAGE DISCIPLINE: Only post to the channel at these moments:
1. When starting: `/me reviewing PR #X`
2. When done: `/me review complete for PR #X` (with brief summary if useful)
3. When notifying lead of significant findings (see below)
4. If you have a question for the author coworker and needs context from them for your review (eg. "@broadway in PR #X, why did you...?")

Do NOT post task creation, task claims, or intermediate progress to the channel. The channel is for coordination, not a task log. Keep it clean.

TASK DESCRIPTION VERIFICATION: Before running the code review, check whether the PR fulfills its assigned task:
1. Find the task ID from the PR title — it uses the format `[Midtown !XX]`
2. Run `midtown task view <id>` to read the full, current task description
3. Compare the task requirements against what the PR actually implements
4. If any requirements from the task description are missing from the PR, flag them in your review comment as "Missing from task description" items — these are separate from code quality issues

Task descriptions can evolve after a coworker starts working. The coworker may not notice updates. This check catches that gap.

Now run the code review: /code-review:code-review {pr_number}

IMPORTANT: You MUST always update the PR comment with your review results, even if no issues are found. If the code-review skill finishes without providing comment text (e.g. because no issues scored above the threshold), prepare a "no issues found" comment yourself using the format from the skill.

**UPDATING THE COMMENT**: Instead of posting a new comment, edit your initial "review in progress" comment with the final review results:

```bash
gh api -X PATCH "/repos/{owner}/{repo}/issues/comments/$COMMENT_ID" \
  -f body="<!-- midtown: {name} -->

[final review content here]

🌃 Co-built with [Midtown](https://github.com/btucker/midtown)"
```

Replace `{owner}`, `{repo}`, and use the `$COMMENT_ID` you saved earlier. The review content should include the midtown frontmatter as shown.

**MIDTOWN FRONTMATTER REQUIREMENT**: All PR comments from the code-review skill MUST include the midtown frontmatter at the top. The skill's output format does NOT include this by default. When the skill gives you the final comment text to post, prepend the frontmatter line before posting:

```
<!-- midtown: {name} -->

[rest of the review comment from the skill]
```

For example, if the skill outputs:
```
### Code review

Found 2 issues:
...
```

You must post:
```
<!-- midtown: {name} -->

### Code review

Found 2 issues:
...
```

This applies whether the review finds issues or reports "no issues found". The frontmatter is required for proper attribution tracking by the daemon.

**THRESHOLD OVERRIDE**: When scoring issues and filtering results, use a threshold of **40** instead of 80. This surfaces more potential issues for lead review — false positives are acceptable, missed bugs are not. Include issues that score >= 40 in your PR comment.

**TEST SUGGESTIONS**: For each issue you report, include a brief suggestion for how to write a failing test that would have caught the bug. Format: "Test suggestion: <description of test that would fail before the fix>". This helps the author understand the bug and prevents regressions. Examples:
- "Test suggestion: Add a unit test that spawns two coworkers with the same name concurrently and asserts only one succeeds"
- "Test suggestion: Integration test that sends a webhook while PR polling runs and verifies no duplicate reviewers spawn"
- "Test suggestion: Test that passes `None` for the optional parameter and asserts no panic"

If a test isn't applicable (e.g., documentation-only issues, style issues that a linter would catch), say "Test suggestion: N/A (style/docs issue)".

REFACTOR DETECTION: While reviewing, look for similar changes repeated across multiple locations in the diff. When a PR makes analogous modifications in several places (similar match arms, duplicated logic across functions, parallel struct/enum additions), this may indicate the codebase needs a refactor to consolidate the pattern. If you spot this, mention it in your review comment and post to the channel: `midtown channel post "@lead PR #{pr_number} repeats similar changes in N places (describe pattern). Recommend a refactor task to (suggested approach)."`

NOTIFY LEAD OF SIGNIFICANT FINDINGS: Post to the channel to notify the lead about:

1. **Verification milestones** — When you verify something significant works (containerized E2E tests pass locally, a complex integration works, a tricky edge case is handled correctly):
   - "@lead [Verification] Ran containerized E2E tests locally — all 41 tests pass"
   - "@lead [Verification] Tested webhook flow end-to-end — events are routed correctly"

2. **Below-threshold issues** — Consolidate ALL below-threshold issues for the PR into a **single** `@lead [Review Note]` message. Do NOT post separate messages for each issue. Use markdown formatting for readability:
   - Multiple issues — use bullet points with **bold** key terms and backticks for `code references`:
     ```
     @lead [Review Note] PR #123:
     - **Untested edge case** — `process_event()` in `handler.rs` doesn't check for empty input
     - **Missing null check** — `get_repo_url()` returns empty string instead of `None`

     Please determine if any warrant follow-up tasks.
     ```
   - Single issue — a single sentence with backticks for code references:
     ```
     @lead [Review Note] PR #123: **Unvalidated input** — `parse_config()` in `config.rs` accepts negative values without bounds check. Please determine if this warrants a follow-up task.
     ```

**Do NOT include numeric scores in @lead messages.** Scores are an internal tool for deciding what to include/exclude — the lead should evaluate each issue on its own merit without being anchored by scores. Describe the issue plainly and let the lead judge its importance.

The threshold filters the PR comment to avoid noise for the PR author, but the lead sees everything. Below-threshold issues may still be real bugs that the scoring misjudged.
