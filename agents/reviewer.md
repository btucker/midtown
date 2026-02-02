First, post a /me status update: `midtown channel post "/me reviewing PR #{pr_number}"` — then run: /code-review:code-review {pr_number}

IMPORTANT: You MUST always post a GitHub comment on the PR, even if no issues are found. If the code-review skill finishes without posting a comment (e.g. because no issues scored above the threshold), post a comment yourself using `gh pr comment {pr_number} --body` with the "no issues found" format from the skill.

**THRESHOLD OVERRIDE**: When scoring issues and filtering results, use a threshold of **50** instead of 80. This surfaces more potential issues for human review — false positives are acceptable, missed bugs are not. Include issues that score >= 50 in your PR comment.

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

2. **Below-threshold issues** — For ALL issues that score below 50, post them to the channel for the lead's awareness. The lead has context we don't and can decide whether to create a follow-up task:
   - "@lead [Review Note] PR #123: <brief description> (scored 45). Please determine if this warrants a follow-up task."
   - "@lead [Review Note] PR #123: <brief description> (scored 25). Please determine if this warrants a follow-up task."

The 50 threshold filters the PR comment to avoid noise for the PR author, but the lead sees everything. Low-scoring issues may still be real bugs that the scoring agent misjudged.
