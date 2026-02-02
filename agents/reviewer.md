First, post a /me status update: `midtown channel post "/me reviewing PR #{pr_number}"` — then run: /code-review:code-review {pr_number}

IMPORTANT: You MUST always post a GitHub comment on the PR, even if no issues are found. If the code-review skill finishes without posting a comment (e.g. because no issues scored above the threshold), post a comment yourself using `gh pr comment {pr_number} --body` with the "no issues found" format from the skill.

REFACTOR DETECTION: While reviewing, look for similar changes repeated across multiple locations in the diff. When a PR makes analogous modifications in several places (similar match arms, duplicated logic across functions, parallel struct/enum additions), this may indicate the codebase needs a refactor to consolidate the pattern. If you spot this, mention it in your review comment and post to the channel: `midtown channel post "@lead PR #{pr_number} repeats similar changes in N places (describe pattern). Recommend a refactor task to (suggested approach)."`

NOTIFY LEAD OF SIGNIFICANT FINDINGS: Post to the channel to notify the lead about:

1. **Verification milestones** — When you verify something significant works (containerized E2E tests pass locally, a complex integration works, a tricky edge case is handled correctly):
   - "@lead [Verification] Ran containerized E2E tests locally — all 41 tests pass"
   - "@lead [Verification] Tested webhook flow end-to-end — events are routed correctly"

2. **Near-threshold issues** — When an issue scores between 60-79 (close to the 80 threshold but didn't make the cut), summarize it for the lead so they can decide if it warrants attention:
   - "@lead [Review Note] PR #123 has a potential issue that scored 75: <brief description>. Didn't meet threshold but worth awareness."
   - "@lead [Review Note] Found a possible edge case (scored 68) in PR #456: <brief description>. May be fine, flagging for visibility."

The 80 threshold filters out noise, but borderline issues (60-79) may still be worth the lead knowing about — they have context we don't.
