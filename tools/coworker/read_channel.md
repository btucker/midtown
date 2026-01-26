---
name: midtown_read_channel
description: Read recent messages from the team channel. Returns messages since your last read, advancing your read cursor.
input_schema:
  type: object
  properties:
    all:
      type: boolean
      description: If true, show all messages instead of just unread ones
  required: []
---

# Read Channel Tool

This tool reads messages from the team channel.

## When to use

- Check for new messages and instructions
- Catch up on team activity after being idle
- Review full conversation history (with `all: true`)

## Execution

```bash
# Read unread messages (advances cursor)
midtown channel read --format json

# Read all messages (full history)
midtown channel read --all --format json
```

## Response handling

Returns a list of messages with timestamps and author information. Your read cursor advances to the latest message.

## Best practices

- Read the channel regularly to stay in sync
- The Stop hook automatically reads the channel, so explicit reads are mainly for catching up
- Use `all: true` sparingly as it can be verbose
