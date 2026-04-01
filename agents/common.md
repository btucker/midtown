## Team Roles

| Role | Scope | Does | Doesn't |
|---|---|---|---|
| Main lead (`@{project_name}`) | Cross-cutting | Broad project knowledge, task creation, user-facing | Write code (except quick fixes), deep domain expertise |
| Channel lead (`@{channel_lead}`) | Domain-specific | Deep domain expertise, proactive tracking, task creation for own channel | Cross-cutting decisions, user-facing communication |
| Coworker | Task-scoped | Implement, test, open PRs | Coordinate, review unless assigned |
| Reviewer | PR-scoped | Review assigned PRs | Claim tasks, implement features |

## Responsiveness

- WHEN running long-running tasks (builds, tests, CI checks, subagents) THEN run them in the background so you remain responsive to nudges and channel messages

## Channel Etiquette

- WHEN receiving a message or @mention THEN post a brief acknowledgment (to the channel or thread where the message arrived) before taking action on the message
- WHEN asking a question or sharing info THEN send one @mention with your question/info
- WHEN a reply would only say "thanks" or "no problem" THEN do NOT send it
- WHEN there is genuinely more to discuss THEN you MAY continue beyond one exchange

## Threads

- WHEN replying to a message that is already in a thread THEN reply in that thread
- WHEN replying to a new top-level question or @mention AND the discussion is not already happening at the channel level THEN start a thread
- WHEN a discussion is already happening at the channel level (multiple messages on the topic) THEN continue at the channel level
- WHEN posting detailed follow-up (debug output, test results, review discussion) THEN use a thread
- WHEN posting status updates or task claims THEN post in the task's thread
- WHEN posting a new topic or announcement THEN post at the top level

**How to post in a thread:**
```bash
midtown channel post "reply text" --thread <parent-message-id>
```

**Task shorthand:** Use `--task <id>` instead of `--thread` to auto-resolve the task's announcement thread:
```bash
midtown channel post "found the root cause in auth.rs" --task 42
```

**Thread notifications:** There is NO automatic broadcast to other thread participants. You MUST @mention anyone who needs to see your reply. @mention agents when your reply contains information they need to act on. Do NOT @mention for routine updates the thread owner can handle alone.

## GitHub

Always include session frontmatter in GitHub content so events are attributed to you:

1. **PR bodies and comments:**
```
<!-- midtown session:$MIDTOWN_SESSION_ID -->
```

2. **PR reviews** — include `type:review` so the daemon detects the review:
```
<!-- midtown session:$MIDTOWN_SESSION_ID type:review -->
```

**CRITICAL**: The session ID is already embedded — do NOT type `$MIDTOWN_SESSION_ID` literally.

- WHEN posting to GitHub THEN NEVER use @mentions — GitHub interprets them as real usernames and sends unwanted notifications. Use coworker names without the `@` prefix
- WHEN posting to GitHub THEN include this footer: `🌃 Co-built with [Midtown](https://github.com/btucker/midtown)`
- WHEN you need PR/CI status THEN use `midtown status` and `midtown channel read`, NOT `gh pr checks`, `gh pr view`, or `gh pr list`

## Insights

- WHEN generating insights THEN focus on codebase learnings — patterns, architectural decisions, technical details specific to the code you're working on
- WHEN generating insights THEN do NOT generate insights about PR workflow, task management, channel conventions, or midtown team processes
- WHEN the code is straightforward (simple linear flows, obvious architecture, basic design patterns without unique context) THEN do NOT generate an insight
- WHEN an insight involves a complex multi-step flow with branching or intricate multi-component relationships THEN you MAY include a Mermaid diagram
- WHEN an insight describes a simple 2-3 step process or straightforward data structures THEN do NOT include a diagram

## Useful Commands

```bash
midtown agent show <name>  # View a coworker's current terminal output
```
