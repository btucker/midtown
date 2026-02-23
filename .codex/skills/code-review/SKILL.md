---
name: code-review
description: |
  Review a GitHub pull request for bugs, CLAUDE.md compliance, and code quality.
  Use when asked to "review PR", "code review", "/code-review", or when assigned
  a review task by the daemon. Syntax: /code-review <PR_NUMBER>
---

# Code Review for Pull Requests

Provide a thorough code review for the specified pull request.

## Prerequisites

- You must have `gh` CLI installed and authenticated
- You must be in a git repository with GitHub remote

## Process

Follow these steps precisely:

### Step 1: Check Eligibility

Use `gh pr view <PR_NUMBER> --json state,isDraft,author,title,reviews` to check:

- Is the PR closed? → Stop, report "PR is closed"
- Is it a draft? → Stop, report "PR is a draft"
- Is it automated (dependabot, renovate)? → Stop, report "Automated PR"
- Already reviewed by you (look for your signature in reviews)? → Stop, report "Already reviewed"

If eligible, proceed to Step 2.

### Step 2: Find CLAUDE.md Files

Find coding standards files:

1. Check for root `CLAUDE.md` in the repository
2. Get modified files: `gh pr view <PR_NUMBER> --json files --jq '.files[].path'`
3. For each unique directory in the modified files, check each parent directory until root and
   collect the first `CLAUDE.md` found in that path
4. Keep the list of found CLAUDE.md files for reference in later steps

```bash
MODIFIED_FILES=$(gh pr view <PR_NUMBER> --json files --jq '[.files[].path][]')
CLAUDE_FILES=$(mktemp)

printf '%s\n' "$MODIFIED_FILES" | while IFS= read -r file; do
    dir=$(dirname "$file")
    if [ "$dir" = "." ]; then
        dir=""
    fi

    cursor="$dir"
    while :; do
        search_dir="${cursor:-.}"
        if [ -f "$search_dir/CLAUDE.md" ]; then
            echo "$search_dir/CLAUDE.md"
            break
        fi

        if [ -z "$cursor" ] || [ "$cursor" = "." ] || [ "$cursor" = "/" ]; then
            break
        fi

        cursor=$(dirname "$cursor")
    done
done | sort -u > "$CLAUDE_FILES"

echo "CLAUDE files:"
cat "$CLAUDE_FILES"
```

5. Keep the list of found CLAUDE.md files for reference in later steps.

### Step 3: Get PR Summary

Run these commands to understand the PR:

```bash
gh pr view <PR_NUMBER>
gh pr diff <PR_NUMBER>
```

Capture:

- PR title and description
- Files changed
- Scope of the change

### Step 4: Run Five Review Passes

For each pass, examine the changes and note any issues found:

**Pass A: CLAUDE.md Compliance**

- Read the CLAUDE.md files found in Step 2
- Check if changes follow the project conventions documented there
- Note: CLAUDE.md is guidance for writing code, so not all rules apply to review
- Focus on violations that directly impact code quality or maintainability

**Pass B: Bug Scan (Shallow)**

- Look for logic errors that would cause incorrect behavior
- Check for missing null/error handling that would cause crashes
- Watch for race conditions or deadlocks
- Focus on LARGE bugs only in the changed lines
- Ignore: nitpicks, style issues, things a linter would catch

**Pass C: Git History Context**

For significantly modified files:

```bash
git log --oneline -10 -- <file>
git blame <file>
```

- Check if PR undoes recent fixes
- Look for patterns that were intentionally established
- Note any historical context that conflicts with the changes

**Pass D: Previous PR Feedback**

Find recent PRs that touched these files:

```bash
git log --oneline -20 -- <files>
```

For relevant commits, check if there was PR feedback that might also apply here.

**Pass E: Code Comment Compliance**

For modified files, read the existing code comments:

- Check TODO/FIXME comments that might be affected
- Verify compliance with documented invariants
- Ensure safety comments for unsafe code are respected
- Check that changes don't violate documented requirements

### Step 5: Score Issues

For each issue found, assign a confidence score (0-100):

| Score | Meaning |
|-------|---------|
| 0 | False positive, pre-existing issue, or doesn't stand up to scrutiny |
| 25 | Might be real, but can't verify. Stylistic issue not in CLAUDE.md |
| 50 | Verified real, but a nitpick or low importance relative to PR |
| 75 | Verified real, important, will impact functionality, or explicitly in CLAUDE.md |
| 100 | Absolutely certain, will happen frequently in practice |

For CLAUDE.md issues: double-check that CLAUDE.md actually calls out the specific issue.

### Step 6: Filter and Re-check

1. Keep only issues with score >= 80
2. If no issues meet threshold, prepare "no issues found" message
3. Re-run eligibility check from Step 1 to ensure PR is still open

### Step 7: Post Comment

Get the full git SHA for links:

```bash
HEAD_SHA=$(gh pr view <PR_NUMBER> --json headRefOid --jq '.headRefOid')
```

**If issues found, use this format:**

```
### Code review

Found N issues:

1. <brief description of issue> (CLAUDE.md says "<relevant quote>" OR <reason>)

https://github.com/OWNER/REPO/blob/<FULL_SHA>/path/to/file.py#L10-L15

2. <next issue>...
```

**If no issues found:**

```
### Code review

No issues found. Checked for bugs and CLAUDE.md compliance.
```

**Link format requirements:**

- MUST use full git SHA (not HEAD, not short hash)
- Format: `https://github.com/OWNER/REPO/blob/SHA/path/to/file.ext#Lstart-Lend`
- Include at least 1 line of context before and after the issue

Post the comment:

```bash
gh pr comment <PR_NUMBER> --body "$(cat <<'EOF'
<your formatted comment>
EOF
)"
```

## False Positives to Avoid

Do NOT flag these as issues:

- Pre-existing issues on lines the PR didn't modify
- Issues a linter, typechecker, or compiler would catch
- General code quality issues (unless explicitly required in CLAUDE.md)
- Changes in functionality that appear intentional
- Pedantic nitpicks a senior engineer wouldn't mention
- Issues explicitly silenced by code comments (e.g., lint ignores)

## Completion

After posting the GitHub comment, confirm in the project channel:

```bash
midtown channel post "Posted review on PR #<PR_NUMBER>: https://github.com/OWNER/REPO/pull/<PR_NUMBER>#issuecomment-<COMMENT_ID>"
```

**A code review is NOT complete until:**

1. A GitHub PR comment has been posted (even if "no issues found")
2. The comment URL has been shared in the channel
