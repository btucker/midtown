## Channel Etiquette

Keep channel messages purposeful. Avoid pointless back-and-forth.

**Note:** Role-specific guidelines (e.g., reviewer constraints) take precedence over general etiquette when they are more restrictive.

- **Asking questions or sharing info**: Send one @mention with your question/info
- **Receiving @mentions**: Reply with a brief acknowledgment if needed, then stop
- **No thank-you chains**: Don't reply just to say "thanks!" or "no problem!"

Good:
```
@park The auth tests are flaky, FYI
```
```
@madison Got it, will check
```

Bad:
```
@park The auth tests are flaky, FYI
@madison Thanks for letting me know!
@park No problem!
@madison 👍
```

If there's genuinely more to discuss, continue. Otherwise, one exchange is enough.

## Useful Commands

```bash
midtown coworker view <name>  # View a coworker's current terminal output
```

Use `midtown coworker view` to check on what a coworker is doing without switching tmux windows. This captures and prints the coworker's tmux pane content.

## GitHub Etiquette

**IMPORTANT**: Always include your name in GitHub content so events are attributed to you:

1. **PR bodies** - add frontmatter:
```
<!-- midtown: {name} -->
```

2. **PR comments and reviews** - include your name in the comment:
```
## Code Review by {name}
...
```
or add the HTML comment anywhere in your comment:
```
<!-- midtown: {name} -->
```

**Reviews are comment-based, not formal GitHub reviews.** All coworkers share the same GitHub user, so `gh pr review --approve` is meaningless. Post reviews as PR comments using `gh pr comment`. Authors merge their own PRs after review feedback is addressed.

**GitHub footer**: When posting to GitHub (PR descriptions, PR comments, review comments), include this footer instead of the default Claude Code footer:
```
🌃 Co-built with [Midtown](https://github.com/btucker/midtown)
```

**CRITICAL: NEVER use @mentions in GitHub** (PR descriptions, comments, reviews). GitHub interprets `@name` as real GitHub usernames and sends unwanted notifications to strangers. This has already caused incidents. Use coworker names without the `@` prefix in all GitHub content. @mentions are ONLY for the IRC channel where the daemon routes them.

- ❌ GitHub: "@park Addressed both issues" — this pings a real GitHub user named "park"
- ✅ GitHub: "Addressed both issues (per park's feedback)"
- ✅ Channel: "@park please check the tests"

## Insights

When generating insights (if enabled by output style settings), focus on **codebase learnings** - interesting patterns, architectural decisions, or technical details specific to the code you're working with.

**Do NOT generate insights about:**
- PR review workflow or process observations
- Task management patterns
- Channel communication conventions
- General midtown team processes

Insights should help users understand the *codebase*, not the *workflow*.
