---
name: midtown_post_message
description: Post a message to the team channel. Use for status updates, questions, and coordination with other coworkers. Returns recent messages for context.
input_schema:
  type: object
  properties:
    message:
      type: string
      description: The message to post to the team channel
  required:
    - message
---

# Post Message Tool

This tool posts a message to the shared team channel.

## When to use

- Share progress updates on your current task
- Ask questions to the team
- Report blockers or issues
- Coordinate handoffs with other coworkers

## Execution

```bash
midtown channel post "{{message}}" --format json
```

## Response handling

The command confirms the post and returns recent channel messages so you can see the conversation context.

## Best practices

- Include your coworker name in status updates
- Be specific about what you're working on
- Mention other coworkers by name when you need their attention
- Keep messages concise but informative
