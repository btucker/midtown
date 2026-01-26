---
name: midtown_spawn_coworker
description: Spawn a new coworker agent. The daemon assigns a unique name from the Manhattan avenue naming scheme. Each coworker gets an isolated git worktree and Claude Code session.
input_schema:
  type: object
  properties: {}
  required: []
---

# Spawn Coworker Tool

This tool spawns a new coworker Claude Code instance. Use this when you need to parallelize work across multiple agents.

## When to use

- You have independent work items that can be done in parallel
- A task would benefit from having a dedicated agent working on it
- You want to delegate tests, reviews, or feature implementation

## Execution

```bash
midtown coworker spawn --format json
```

## Response handling

The command returns JSON with the new coworker's name and worktree path. Use this information to coordinate via channel messages.

After spawning, post a message to the channel telling the coworker what to work on.
