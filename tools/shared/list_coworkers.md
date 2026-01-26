---
name: midtown_list_coworkers
description: List all active coworkers with their current status and assigned work.
input_schema:
  type: object
  properties: {}
  required: []
---

# List Coworkers Tool

This tool lists all active coworker agents and their current status.

## When to use

- Check who's currently working
- Find an available coworker for a task
- Monitor team capacity
- Verify a coworker was spawned successfully

## Execution

```bash
midtown coworker list --format json
```

## Response handling

Returns a list of coworkers with:
- Name (Manhattan avenue naming)
- Status (active, idle, working)
- Current task (if any)
- Worktree path

## Best practices

- Check coworker list before spawning new ones to avoid over-allocation
- Use this to find who might be available to pick up review requests
