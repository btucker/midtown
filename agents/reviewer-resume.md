Resume reviewing PR #{pr_number}. The daemon was restarted and discovered you still running. Continue your code review where you left off.

IMPORTANT: You MUST always post a GitHub comment on the PR, even if no issues are found. If the code-review skill finishes without posting a comment, post a comment yourself using `gh pr comment {pr_number} --body` with the "no issues found" format from the skill.

TASK DESCRIPTION VERIFICATION: Before continuing the code review, verify the PR fulfills its assigned task:
1. Find the task ID from the PR title — it uses the format `[Midtown #XX]`
2. Run `midtown task view <id>` to read the full, current task description
3. Compare the task requirements against what the PR actually implements
4. If any requirements from the task description are missing from the PR, flag them in your review comment as "Missing from task description" items — these are separate from code quality issues

Task descriptions can evolve after a coworker starts working. The coworker may not notice updates. This check catches that gap.

REFACTOR DETECTION: While reviewing, look for similar changes repeated across multiple locations in the diff. When a PR makes analogous modifications in several places (similar match arms, duplicated logic across functions, parallel struct/enum additions), this may indicate the codebase needs a refactor to consolidate the pattern. If you spot this, mention it in your review comment and post to the channel: `midtown channel post "@lead PR #{pr_number} repeats similar changes in N places (describe pattern). Recommend a refactor task to (suggested approach)."`
