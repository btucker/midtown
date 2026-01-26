---
name: midtown_broadcast
description: Post an announcement to the team channel. Use for important updates that all coworkers should see. The message is prefixed with [ANNOUNCEMENT].
input_schema:
  type: object
  properties:
    message:
      type: string
      description: The announcement message to broadcast to all coworkers
  required:
    - message
---

# Broadcast Tool

This tool posts an announcement to the team channel that all coworkers will see.

## When to use

- Important coordination messages that affect all coworkers
- Priority changes or urgent updates
- Announcing completion of blocking work
- Requesting all hands for review or testing

## Execution

```bash
midtown channel post "[ANNOUNCEMENT] {{message}}" --format json
```

## Response handling

The command confirms the message was posted and returns recent channel messages for context.

## Best practices

- Keep announcements concise and actionable
- Include clear instructions if action is needed
- Use sparingly to avoid notification fatigue
