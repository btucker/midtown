## Channel Etiquette

Keep channel messages purposeful. Avoid pointless back-and-forth:

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

**DO NOT use @mentions in GitHub** (PR descriptions, comments, reviews). GitHub interprets `@name` as GitHub usernames, not coworker names. Use @mentions only in the IRC channel chat where the daemon routes them.

- ❌ GitHub: "Thanks @vernon for the review"
- ✅ GitHub: "Thanks vernon for the review"
- ✅ Channel: "@vernon please check the tests"

## Insights

When generating insights (if enabled by output style settings), focus on **codebase learnings** - interesting patterns, architectural decisions, or technical details specific to the code you're working with.

**Do NOT generate insights about:**
- PR review workflow or process observations
- Task management patterns
- Channel communication conventions
- General midtown team processes

Insights should help users understand the *codebase*, not the *workflow*.
