Resume reviewing PR #{pr_number}. The daemon was restarted and discovered you still running. Continue your code review where you left off.

CHECK FOR EXISTING REVIEW COMMENT: First, check if you already posted an initial "review in progress" comment:

```bash
gh pr view {pr_number} --json comments --jq '.comments[] | select(.body | contains("Review in progress by {name}")) | .id' | head -1
```

If a comment ID is returned, save it as `COMMENT_ID` for later editing. If not, post the initial comment now:

```bash
COMMENT_URL=$(gh pr comment {pr_number} --body "## Review Status

🔍 Review in progress by {name}...

---
> [!NOTE]
> This comment will be updated with the review results when complete.

🌃 Co-built with [Midtown](https://github.com/btucker/midtown)")
COMMENT_ID=$(echo "$COMMENT_URL" | grep -o '[0-9]*$')
```

**WHY NO FRONTMATTER AND DIFFERENT HEADING**: The initial comment deliberately:
1. Omits `<!-- midtown: {name} -->` frontmatter
2. Uses "Review Status" instead of "Code Review" as the heading

This prevents the daemon from marking the PR as "reviewed" before the review is actually complete. The frontmatter and correct heading will be added when you update the comment with the final review results.

IMPORTANT: You MUST always update the PR comment with your review results, even if no issues are found. If the code-review skill finishes without providing comment text, prepare a "no issues found" comment yourself using the format from the skill.

TASK DESCRIPTION VERIFICATION: Before continuing the code review, verify the PR fulfills its assigned task:
1. Find the task ID from the PR title — it uses the format `[Midtown !XX]`
2. Run `midtown task view <id>` to read the full, current task description
3. Compare the task requirements against what the PR actually implements
4. If any requirements from the task description are missing from the PR, flag them in your review comment as "Missing from task description" items — these are separate from code quality issues

Task descriptions can evolve after a coworker starts working. The coworker may not notice updates. This check catches that gap.

If you haven't already run the code review skill, run it now: /code-review:code-review {pr_number}

**UPDATING THE COMMENT**: Instead of posting a new comment, edit your initial "review in progress" comment with the final review results:

```bash
gh api -X PATCH "/repos/{owner}/{repo}/issues/comments/$COMMENT_ID" \
  -f body="<!-- midtown: {name} -->

[final review content here]

🌃 Co-built with [Midtown](https://github.com/btucker/midtown)"
```

Replace `{owner}`, `{repo}`, and use the `$COMMENT_ID` you saved earlier. The review content should include the midtown frontmatter as shown.

**MIDTOWN FRONTMATTER REQUIREMENT**: All PR comments from the code-review skill MUST include the midtown frontmatter at the top. The skill's output format does NOT include this by default. When the skill gives you the final comment text to post, prepend the frontmatter line before editing the comment:

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

REFACTOR DETECTION: While reviewing, look for similar changes repeated across multiple locations in the diff. When a PR makes analogous modifications in several places (similar match arms, duplicated logic across functions, parallel struct/enum additions), this may indicate the codebase needs a refactor to consolidate the pattern. If you spot this, mention it in your review comment and post to the channel: `midtown channel post "@{project_name} PR #{pr_number} repeats similar changes in N places (describe pattern). Recommend a refactor task to (suggested approach)."`
