---
name: midtown_shutdown_coworker
description: Gracefully shutdown a coworker agent by name. The coworker will finish current work and exit.
input_schema:
  type: object
  properties:
    name:
      type: string
      description: Name of the coworker to shutdown (e.g., 'broadway', 'madison')
  required:
    - name
---

# Shutdown Coworker Tool

This tool gracefully shuts down a specific coworker agent.

## When to use

- A coworker has completed their assigned work
- You need to reclaim resources
- A coworker is stuck or no longer needed

## Execution

```bash
midtown coworker shutdown "{{name}}" --format json
```

## Response handling

The command confirms the shutdown. The coworker's worktree may be cleaned up depending on configuration.

Note: If the coworker has uncommitted changes, consider coordinating via channel first to ensure work is saved.
