---
name: midtown_claim_task
description: Claim a task by ID. This marks you as the owner and prevents others from working on it.
input_schema:
  type: object
  properties:
    task_id:
      type: string
      description: The task ID to claim
  required:
    - task_id
---

# Claim Task Tool

This tool claims ownership of a task, preventing other coworkers from working on it.

## When to use

- Before starting work on a task
- When you see an unclaimed task you can handle
- To reserve work during coordination

## Execution

```bash
midtown task claim "{{task_id}}" --format json
```

## Response handling

The command confirms the claim and returns the task details. If the task is already claimed by another coworker, the claim will fail.

## Best practices

- Always claim a task before starting work on it
- Release tasks you can't complete (by posting to channel)
- Check task list regularly for unclaimed work
