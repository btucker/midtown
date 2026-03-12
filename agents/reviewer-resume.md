Resume reviewing PR #{pr_number}. The daemon was restarted and discovered you still running. Continue your code review where you left off.

**NOTE**: The daemon manages the placeholder comment on the PR. You do not need to post or track a comment ID. When your review is complete, use `midtown pr review post` to submit your findings.

IMPORTANT: You MUST complete and submit your review findings, even if no issues are found. If the code-review skill finishes without providing comment text, prepare a "no issues found" comment yourself using the format from the skill.

TASK DESCRIPTION VERIFICATION: Before continuing the code review, verify the PR fulfills its assigned task:
1. Find the task ID from the PR title — it uses the format `[Midtown !XX]`
2. Run `midtown task view <id>` to read the full, current task description
3. Compare the task requirements against what the PR actually implements
4. If any requirements from the task description are missing from the PR, flag them in your review comment as "Missing from task description" items — these are separate from code quality issues

Task descriptions can evolve after a coworker starts working. The coworker may not notice updates. This check catches that gap.

If you haven't already run the code review skill, run it now: {code_review_invocation}

**POSTING YOUR REVIEW**: When your review is complete, write your findings to a temp file and submit via the CLI. The daemon handles the placeholder comment update, frontmatter, and footer automatically.

```bash
cat > /tmp/review-{pr_number}.md << 'REVIEW_EOF'
[your review content here — no frontmatter or footer needed]
REVIEW_EOF

midtown pr review post --pr {pr_number} --body-file /tmp/review-{pr_number}.md

# Cross-post review to the task thread so the team sees it inline
midtown channel post "$(cat /tmp/review-{pr_number}.md)" --task {task_id}
```

**IMPORTANT**: Do NOT include `<!-- midtown: {name} -->` frontmatter or the Midtown footer in your review content. The daemon adds these automatically.

REFACTOR DETECTION: While reviewing, look for similar changes repeated across multiple locations in the diff. When a PR makes analogous modifications in several places (similar match arms, duplicated logic across functions, parallel struct/enum additions), this may indicate the codebase needs a refactor to consolidate the pattern. If you spot this, mention it in your review comment and post to the channel: `midtown channel post "@{project_name} PR #{pr_number} repeats similar changes in N places (describe pattern). Recommend a refactor task to (suggested approach)."`
