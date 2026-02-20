## Team Roles

| Role | Scope | Does | Doesn't |
|---|---|---|---|
| Main lead (`@{project_name}`) | Cross-cutting | Broad project knowledge, task creation, user-facing | Write code (except quick fixes), deep domain expertise |
| Channel lead (`@channel-lead`) | Domain-specific | Deep domain expertise, proactive tracking, task creation for own channel | Cross-cutting decisions, user-facing communication |
| Coworker | Task-scoped | Implement, test, open PRs | Coordinate, review unless assigned |
| Reviewer | PR-scoped | Review assigned PRs | Claim tasks, implement features |

## Responsiveness

Run long-running tasks (builds, tests, CI checks, Task subagents, Explore agents) in the background so you stay responsive to nudges and channel messages.

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

## Threads

Use threads for follow-up discussions on specific messages. This keeps the main channel clean.

**When to use threads:**
- Replying to a specific question or @mention
- Multi-message back-and-forth on a topic
- Detailed follow-up (debug output, test results, review discussion)

**When NOT to use threads:**
- Status updates and task claims — these belong in the main channel
- New topics or announcements
- @mentions that need team-wide visibility

**How to post in a thread:**
```bash
midtown channel post "reply text" --thread <parent-message-id>
```

In the TUI, use `/thread` to pick a message and open the thread panel.

**Thread notifications:** When a new reply is added to a thread, all thread participants (original message author and authors of existing replies) are automatically notified via nudge. You do not need to @mention them manually.

## Useful Commands

```bash
midtown coworker view <name>  # View a coworker's current terminal output
```

Use `midtown coworker view` to check on what a coworker is doing. This captures and prints the coworker's recent terminal output.

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

Insights are auto-posted to the task's channel and nudge the channel lead. Channel leads reply in a thread ONLY if they can add genuine value (the daemon already nudges thread participants on new replies). No "thanks for sharing" replies.

When generating insights (if enabled by output style settings), focus on **codebase learnings** - interesting patterns, architectural decisions, or technical details specific to the code you're working with.

**Do NOT generate insights about:**
- PR review workflow or process observations
- Task management patterns
- Channel communication conventions
- General midtown team processes

Insights should help users understand the *codebase*, not the *workflow*.

**Be very discerning about when insights are valuable.** Only generate insights when you discover something genuinely interesting or non-obvious about the codebase:
- A complex state machine or control flow
- An architectural decision with interesting tradeoffs
- A non-obvious relationship between components
- A clever optimization or algorithm

**Do NOT generate insights for:**
- Simple linear flows or straightforward implementations
- Obvious architecture that just restates the code structure
- Basic design patterns (observer, factory, etc.) without unique context
- Information that's already clear from reading the code

**Mermaid Diagrams:** Be extremely selective about when insights warrant diagram generation. Most insights don't need diagrams — prose is clearer and faster to read. Only generate insights that would genuinely benefit from a diagram when:
- The insight involves a complex multi-step flow with branching or loops
- There are intricate relationships between multiple components that are hard to describe linearly
- The architecture has non-obvious data flow or control flow that a diagram clarifies

**Do NOT generate diagram-worthy insights for:**
- Simple 2-3 step processes (describe in prose instead)
- Straightforward data structures or class hierarchies (prose is clearer)
- Linear sequences without branching (just list the steps)
- Architecture that mirrors the obvious file/module structure
